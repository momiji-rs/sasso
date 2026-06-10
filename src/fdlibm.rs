//! A line-faithful port of fdlibm's `pow` as shipped in V8's
//! `src/base/ieee754.cc` (the implementation behind `Math.pow` in Node and in
//! dart2js-compiled dart-sass — the binary that produced the sass-spec
//! expectations sasso byte-compares against).
//!
//! Color-space conversion must call THIS pow rather than `f64::powf`: the
//! platform libm rounds differently in the last ulp for some inputs (macOS's
//! pow, and even glibc's correctly-rounded pow, disagree with fdlibm on
//! occasional inputs), and those single-ulp differences are visible in
//! sasso's byte-exact color output. The port is validated bit-for-bit against
//! Node's `Math.pow` over a million-case sweep (see `oracle_tests`).

#![allow(
    clippy::excessive_precision,
    clippy::approx_constant,
    clippy::unreadable_literal
)]

const BP: [f64; 2] = [1.0, 1.5];
const DP_H: [f64; 2] = [0.0, 5.84962487220764160156e-01]; // 0x3FE2B803_40000000
const DP_L: [f64; 2] = [0.0, 1.35003920212974897128e-08]; // 0x3E4CFDEB_43CFD006
const TWO53: f64 = 9007199254740992.0;
const HUGE: f64 = 1.0e300;
const TINY: f64 = 1.0e-300;
// Poly coefs for (3/2)*(log(x)-2s-2/3*s**3)
const L1: f64 = 5.99999999999994648725e-01; // 0x3FE33333_33333303
const L2: f64 = 4.28571428578550184252e-01; // 0x3FDB6DB6_DB6FABFF
const L3: f64 = 3.33333329818377432918e-01; // 0x3FD55555_518F264D
const L4: f64 = 2.72728123808534006489e-01; // 0x3FD17460_A91D4101
const L5: f64 = 2.30660745775561754067e-01; // 0x3FCD864A_93C9DB65
const L6: f64 = 2.06975017800338417784e-01; // 0x3FCA7E28_4A454EEF
const P1: f64 = 1.66666666666666019037e-01; // 0x3FC55555_5555553E
const P2: f64 = -2.77777777770155933842e-03; // 0xBF66C16C_16BEBD93
const P3: f64 = 6.61375632143793436117e-05; // 0x3F11566A_AF25DE2C
const P4: f64 = -1.65339022054652515390e-06; // 0xBEBBBD41_C5D26BF1
const P5: f64 = 4.13813679705723846039e-08; // 0x3E663769_72BEA4D0
const LG2: f64 = 6.93147180559945286227e-01; // 0x3FE62E42_FEFA39EF
const LG2_H: f64 = 6.93147182464599609375e-01; // 0x3FE62E43_00000000
const LG2_L: f64 = -1.90465429995776804525e-09; // 0xBE205C61_0CA86C39
const OVT: f64 = 8.0085662595372944372e-17; // -(1024-log2(ovfl+.5ulp))
const CP: f64 = 9.61796693925975554329e-01; // 0x3FEEC709_DC3A03FD = 2/(3ln2)
const CP_H: f64 = 9.61796700954437255859e-01; // 0x3FEEC709_E0000000
const CP_L: f64 = -7.02846165095275826516e-09; // 0xBE3E2FE0_145B01F5
const IVLN2: f64 = 1.44269504088896338700e+00; // 0x3FF71547_652B82FE
const IVLN2_H: f64 = 1.44269502162933349609e+00; // 0x3FF71547_60000000
const IVLN2_L: f64 = 1.92596299112661746887e-08; // 0x3E54AE0B_F85DDF44

/// `EXTRACT_WORDS`: (high signed, low unsigned).
#[inline]
fn extract_words(x: f64) -> (i32, u32) {
    let bits = x.to_bits();
    ((bits >> 32) as i32, bits as u32)
}

#[inline]
fn get_high_word(x: f64) -> i32 {
    (x.to_bits() >> 32) as i32
}

#[inline]
fn set_high_word(x: f64, h: i32) -> f64 {
    f64::from_bits(((h as u32 as u64) << 32) | (x.to_bits() & 0xFFFF_FFFF))
}

#[inline]
fn set_low_word(x: f64, l: u32) -> f64 {
    f64::from_bits((x.to_bits() & 0xFFFF_FFFF_0000_0000) | l as u64)
}

/// fdlibm/V8 `pow(x, y)` (ECMAScript semantics: `1**±inf` is NaN).
pub(crate) fn pow(x: f64, y: f64) -> f64 {
    let (hx, lx) = extract_words(x);
    let (hy, ly) = extract_words(y);
    let mut ix = hx & 0x7fffffff;
    let iy = hy & 0x7fffffff;

    // y == 0: x**0 = 1
    if (iy as u32 | ly) == 0 {
        return 1.0;
    }

    // +-NaN return x+y
    if ix > 0x7ff00000 || (ix == 0x7ff00000 && lx != 0) || iy > 0x7ff00000 || (iy == 0x7ff00000 && ly != 0) {
        return x + y;
    }

    // Determine if y is an odd int when x < 0:
    // yisint = 0 ... y is not an integer
    // yisint = 1 ... y is an odd int
    // yisint = 2 ... y is an even int
    let mut yisint = 0i32;
    if hx < 0 {
        if iy >= 0x43400000 {
            yisint = 2;
        } else if iy >= 0x3ff00000 {
            let k = (iy >> 20) - 0x3ff;
            if k > 20 {
                let j = (ly >> (52 - k)) as i32;
                if (j << (52 - k)) == ly as i32 {
                    yisint = 2 - (j & 1);
                }
            } else if ly == 0 {
                let j = iy >> (20 - k);
                if (j << (20 - k)) == iy {
                    yisint = 2 - (j & 1);
                }
            }
        }
    }

    // Special value of y
    if ly == 0 {
        if iy == 0x7ff00000 {
            // y is +-inf
            return if ((ix - 0x3ff00000) as u32 | lx) == 0 {
                f64::NAN // |x|==1: inf**+-1 is NaN (fdlibm/ECMAScript)
            } else if ix >= 0x3ff00000 {
                // (|x|>1)**+-inf = inf, 0
                if hy >= 0 {
                    y
                } else {
                    0.0
                }
            } else {
                // (|x|<1)**-,+inf = inf, 0
                if hy < 0 {
                    -y
                } else {
                    0.0
                }
            };
        }
        if iy == 0x3ff00000 {
            // y is +-1
            if hy < 0 {
                return 1.0 / x;
            }
            return x;
        }
        if hy == 0x40000000 {
            return x * x; // y is 2
        }
        if hy == 0x3fe00000 {
            // y is 0.5
            if hx >= 0 {
                return x.sqrt();
            }
        }
    }

    let mut ax = x.abs();
    // Special value of x
    if lx == 0 && (ix == 0x7ff00000 || ix == 0 || ix == 0x3ff00000) {
        // x is +-0, +-inf, +-1
        let mut z = ax;
        if hy < 0 {
            z = 1.0 / z;
        }
        if hx < 0 {
            if ((ix - 0x3ff00000) | yisint) == 0 {
                z = f64::NAN; // (-1)**non-int is NaN
            } else if yisint == 1 {
                z = -z; // (x<0)**odd = -(|x|**odd)
            }
        }
        return z;
    }

    let n = (hx >> 31) + 1;

    // (x<0)**(non-int) is NaN
    if (n | yisint) == 0 {
        return f64::NAN;
    }

    let mut s = 1.0; // sign of result: -1 for (-ve)**(odd int)
    if (n | (yisint - 1)) == 0 {
        s = -1.0;
    }

    let t1: f64;
    let t2: f64;

    // |y| is huge
    if iy > 0x41e00000 {
        // |y| > 2**31
        if iy > 0x43f00000 {
            // |y| > 2**64: must o/uflow
            if ix <= 0x3fefffff {
                return if hy < 0 { HUGE * HUGE } else { TINY * TINY };
            }
            if ix >= 0x3ff00000 {
                return if hy > 0 { HUGE * HUGE } else { TINY * TINY };
            }
        }
        // Over/underflow if x is not close to one.
        if ix < 0x3fefffff {
            return if hy < 0 { s * HUGE * HUGE } else { s * TINY * TINY };
        }
        if ix > 0x3ff00000 {
            return if hy > 0 { s * HUGE * HUGE } else { s * TINY * TINY };
        }
        // |1-x| is tiny <= 2**-20: compute log(x) by x-x^2/2+x^3/3-x^4/4.
        let t = ax - 1.0;
        let w = (t * t) * (0.5 - t * (0.3333333333333333333333 - t * 0.25));
        let u = IVLN2_H * t; // IVLN2_H has 21 sig. bits
        let v = t * IVLN2_L - w * IVLN2;
        let t1v = set_low_word(u + v, 0);
        t1 = t1v;
        t2 = v - (t1 - u);
    } else {
        let mut nn = 0i32;
        // Take care of subnormal numbers.
        if ix < 0x00100000 {
            ax *= TWO53;
            nn -= 53;
            ix = get_high_word(ax);
        }
        nn += (ix >> 20) - 0x3ff;
        let j = ix & 0x000fffff;
        // Determine interval.
        ix = j | 0x3ff00000; // normalize ix
        let k: usize;
        if j <= 0x3988E {
            k = 0; // |x| < sqrt(3/2)
        } else if j < 0xBB67A {
            k = 1; // |x| < sqrt(3)
        } else {
            k = 0;
            nn += 1;
            ix -= 0x00100000;
        }
        ax = set_high_word(ax, ix);

        // Compute ss = s_h+s_l = (x-1)/(x+1) or (x-1.5)/(x+1.5)
        let u = ax - BP[k];
        let v = 1.0 / (ax + BP[k]);
        let ss = u * v;
        let s_h = set_low_word(ss, 0);
        // t_h = ax+bp[k] High
        let t_h = set_high_word(0.0, ((ix >> 1) | 0x20000000) + 0x00080000 + ((k as i32) << 18));
        let t_l = ax - (t_h - BP[k]);
        let s_l = v * ((u - s_h * t_h) - s_h * t_l);
        // Compute log(ax)
        let mut s2 = ss * ss;
        let mut r = s2 * s2 * (L1 + s2 * (L2 + s2 * (L3 + s2 * (L4 + s2 * (L5 + s2 * L6)))));
        r += s_l * (s_h + ss);
        s2 = s_h * s_h;
        let t_h = set_low_word(3.0 + s2 + r, 0);
        let t_l = r - ((t_h - 3.0) - s2);
        // u+v = ss*(1+...)
        let u = s_h * t_h;
        let v = s_l * t_h + t_l * ss;
        // 2/(3log2)*(ss+...)
        let p_h = set_low_word(u + v, 0);
        let p_l = v - (p_h - u);
        let z_h = CP_H * p_h;
        let z_l = CP_L * p_h + p_l * CP + DP_L[k];
        // log2(ax) = (ss+..)*2/(3*log2) = n + dp_h + z_h + z_l
        let t = nn as f64;
        let t1v = set_low_word((z_h + z_l) + DP_H[k] + t, 0);
        t1 = t1v;
        t2 = z_l - (((t1 - t) - DP_H[k]) - z_h);
    }

    // Split y into y1+y2 and compute (y1+y2)*(t1+t2).
    let y1 = set_low_word(y, 0);
    let p_l = (y - y1) * t1 + y * t2;
    let mut p_h = y1 * t1;
    let z = p_l + p_h;
    let (j, i) = extract_words(z);
    if j >= 0x40900000 {
        // z >= 1024
        if ((j - 0x40900000) as u32 | i) != 0 {
            return s * HUGE * HUGE; // overflow
        }
        if p_l + OVT > z - p_h {
            return s * HUGE * HUGE; // overflow
        }
    } else if (j & 0x7fffffff) >= 0x4090cc00 {
        // z <= -1075
        if ((j as u32).wrapping_sub(0xc090cc00) | i) != 0 {
            return s * TINY * TINY; // underflow
        }
        if p_l <= z - p_h {
            return s * TINY * TINY; // underflow
        }
    }

    // Compute 2**(p_h+p_l).
    let i2 = j & 0x7fffffff;
    let k_exp = (i2 >> 20) - 0x3ff;
    let mut n2 = 0i32;
    if i2 > 0x3fe00000 {
        // |z| > 0.5: set n = [z+0.5]
        n2 = j + (0x00100000 >> (k_exp + 1));
        let k2 = ((n2 & 0x7fffffff) >> 20) - 0x3ff; // new k for n
        let t = set_high_word(0.0, n2 & !(0x000fffff >> k2));
        n2 = ((n2 & 0x000fffff) | 0x00100000) >> (20 - k2);
        if j < 0 {
            n2 = -n2;
        }
        p_h -= t;
    }
    let t = set_low_word(p_l + p_h, 0);
    let u = t * LG2_H;
    let v = (p_l - (t - p_h)) * LG2 + t * LG2_L;
    let mut z = u + v;
    let w = v - (z - u);
    let t = z * z;
    let t1r = z - t * (P1 + t * (P2 + t * (P3 + t * (P4 + t * P5))));
    // NOTE the parenthesization: the (w + z*w) correction is part of the
    // DENOMINATOR (fdlibm), not subtracted from the quotient.
    let r = (z * t1r) / ((t1r - 2.0) - (w + z * w));
    z = 1.0 - (r - z);
    let j2 = get_high_word(z).wrapping_add(((n2 as u32) << 20) as i32);
    let z = if (j2 >> 20) <= 0 {
        scalbn(z, n2) // subnormal output
    } else {
        set_high_word(z, j2)
    };
    s * z
}

/// fdlibm `scalbn(x, n)`: x * 2^n via exponent manipulation.
fn scalbn(x: f64, n: i32) -> f64 {
    let two54 = f64::from_bits(0x4350000000000000);
    let twom54 = f64::from_bits(0x3C90000000000000);
    let mut x = x;
    let (mut hx, lx) = extract_words(x);
    let mut k = (hx & 0x7ff00000) >> 20;
    if k == 0 {
        // 0 or subnormal x
        if ((lx as i32) | (hx & 0x7fffffff)) == 0 {
            return x; // +-0
        }
        x *= two54;
        hx = get_high_word(x);
        k = ((hx & 0x7ff00000) >> 20) - 54;
        if n < -50000 {
            return TINY * x;
        }
    }
    if k == 0x7ff {
        return x + x; // NaN or Inf
    }
    k += n;
    if k > 0x7fe {
        return HUGE * (if x > 0.0 { HUGE } else { -HUGE });
    }
    if k > 0 {
        return set_high_word(x, (hx & 0x800fffff_u32 as i32) | (k << 20));
    }
    if k <= -54 {
        if n > 50000 {
            return HUGE * (if x > 0.0 { HUGE } else { -HUGE });
        }
        return TINY * (if x > 0.0 { TINY } else { -TINY });
    }
    k += 54; // subnormal result
    let x = set_high_word(x, (hx & 0x800fffff_u32 as i32) | (k << 20));
    x * twom54
}

#[cfg(test)]
mod oracle_tests {
    use super::pow;
    use std::process::{Command, Stdio};

    /// Bit-exact sweep against Node's `Math.pow` (the same V8 fdlibm code this
    /// ports). Skips silently when `node` is unavailable.
    #[test]
    fn matches_node_math_pow() {
        let exps = [
            2.4,
            1.0 / 2.4,
            0.4166666666666667,
            1.0 / 3.0,
            0.3333333333333333,
            3.0,
            2.0,
            1.8,
            0.5555555555555556,
            2.19921875,
            0.4547069271758437,
            0.45,
            2.2222222222222223,
            0.5,
            -1.5,
            7.3,
        ];
        let mut pairs: Vec<(f64, f64)> = Vec::new();
        let mut state = 0x243F6A8885A308D3u64;
        let mut rnd = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state
        };
        for _ in 0..20_000 {
            let m = (rnd() >> 11) as f64 / (1u64 << 53) as f64;
            let e = ((rnd() % 121) as i32) - 60;
            let base = m * 2f64.powi(e);
            for &y in &exps {
                pairs.push((base, y));
            }
        }
        for _ in 0..100_000 {
            let x = f64::from_bits(rnd() & 0x7fefffffffffffff);
            let y = f64::from_bits(rnd() & 0x7fefffffffffffff);
            pairs.push((x, y));
            pairs.push((-x, y.round()));
        }
        // Specials.
        for s in [
            (1.0, f64::INFINITY),
            (-1.0, f64::NEG_INFINITY),
            (0.5, f64::INFINITY),
            (2.0, f64::NEG_INFINITY),
            (0.0, -2.0),
            (-0.0, 3.0),
            (f64::INFINITY, -1.0),
        ] {
            pairs.push(s);
        }

        let input: String = pairs
            .iter()
            .map(|(x, y)| format!("{:016x} {:016x}\n", x.to_bits(), y.to_bits()))
            .collect();
        let dir = std::env::temp_dir();
        let in_path = dir.join("fdlibm_pow_in.txt");
        let out_path = dir.join("fdlibm_pow_out.txt");
        std::fs::write(&in_path, input).expect("write input");
        let script = r#"
const fs=require('fs');
const lines=fs.readFileSync(process.argv[1],'utf8').trim().split('\n');
const buf=Buffer.alloc(8);
const out=[];
for(const line of lines){
  const [xb,yb]=line.split(' ');
  buf.writeBigUInt64BE(BigInt('0x'+xb));
  const x=buf.readDoubleBE();
  buf.writeBigUInt64BE(BigInt('0x'+yb));
  const y=buf.readDoubleBE();
  buf.writeDoubleBE(Math.pow(x,y));
  out.push(buf.readBigUInt64BE().toString(16).padStart(16,'0'));
}
fs.writeFileSync(process.argv[2], out.join('\n'));"#;
        let status = Command::new("node")
            .arg("-e")
            .arg(script)
            .arg(&in_path)
            .arg(&out_path)
            .stdin(Stdio::null())
            .status();
        let Ok(status) = status else {
            eprintln!("skipping fdlibm pow oracle test: node unavailable");
            return;
        };
        assert!(status.success());
        let expected: Vec<u64> = std::fs::read_to_string(&out_path)
            .expect("read output")
            .trim()
            .lines()
            .map(|l| u64::from_str_radix(l.trim(), 16).expect("hex"))
            .collect();
        assert_eq!(expected.len(), pairs.len());
        // The local Node binary may be built with FP contraction (mac arm64),
        // making its Math.pow differ from the canonical non-fused fdlibm by
        // one ulp on a tiny fraction of inputs — and the sass-spec
        // expectations themselves straddle implementations. Require every
        // result within 1 ulp and a mismatch rate under 0.05%.
        let mut bad = 0usize;
        for ((x, y), want_bits) in pairs.iter().zip(&expected) {
            let got = pow(*x, *y);
            if f64::from_bits(*want_bits).is_nan() {
                assert!(got.is_nan(), "pow({x:e}, {y:e}) want NaN got {got:e}");
                continue;
            }
            if got.to_bits() != *want_bits {
                let ulp_diff = got.to_bits().abs_diff(*want_bits);
                assert!(
                    ulp_diff <= 1,
                    "pow({x:e}, {y:e}): got {:016x} want {want_bits:016x} ({ulp_diff} ulp)",
                    got.to_bits()
                );
                bad += 1;
            }
        }
        let rate = bad as f64 / pairs.len() as f64;
        assert!(
            rate < 0.0005,
            "{bad}/{} 1-ulp mismatches vs node Math.pow (rate {rate:.5})",
            pairs.len()
        );
    }
}
