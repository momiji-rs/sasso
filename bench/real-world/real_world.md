# Real-world corpus: sasso vs dart-sass

Well-known open-source Sass codebases, each pinned to its default-branch HEAD
as of 2026-07-04 and verified active (last commit within 6 months). Each entry
point is compiled standalone with both engines; output parity is checked after
whitespace + color-serialization canonicalization (`bench/scripts/`).

- dart-sass: `1.101.0 compiled with dart2js 3.12.2` (npm `sass`, invoked via its CLI)
- sasso: `sasso 0.7.0` (release build, this repo)
- timing: hyperfine, 2 warmup + ≥10 runs, full CLI invocation (median)

Process-startup baseline (empty input): dart-sass 144 ms, sasso 2 ms. Full-invocation medians below include this cost — it is what a CLI/CI user experiences per call.

| project | stars | pin | sass sources | dart-sass | sasso | speedup | output parity |
|---|---|---|---|---|---|---|---|
| [bootstrap](https://github.com/twbs/bootstrap) | 174,424 | `d35950e` | 99 files / 344 KB | 621 ms | 93 ms | **6.7×** | byte-identical |
| [bulma](https://github.com/jgthms/bulma) | 50,072 | `741da22` | 182 files / 1078 KB | 1316 ms | 205 ms | **6.4×** | byte-identical |
| [govuk-frontend](https://github.com/alphagov/govuk-frontend) | 1,420 | `cfd2224` | 304 files / 276 KB | 403 ms | 40 ms | **10.1×** | byte-identical |
| [uswds](https://github.com/uswds/uswds) | 7,123 | `95717ff` | 605 files / 800 KB | 3241 ms | 564 ms | **5.7×** | byte-identical |
| [carbon](https://github.com/carbon-design-system/carbon) | 9,258 | `f1d6bbf` | 291 files / 1071 KB | — | — | **—** | excluded (see projects.json) |
| [primer-css](https://github.com/primer/css) | 12,983 | `17fa611` | 113 files / 216 KB | 521 ms | 34 ms | **15.1×** | byte-identical |
| [vuetify](https://github.com/vuetifyjs/vuetify) | 41,009 | `998fe6a` | 44 files / 76 KB | 491 ms | 37 ms | **13.3×** | byte-identical |
| [quasar](https://github.com/quasarframework/quasar) | 27,189 | `4202048` | 92 files / 223 KB | 492 ms | 181 ms | **2.7×** | byte-identical |
| [mastodon](https://github.com/mastodon/mastodon) | 50,088 | `163f96c` | 31 files / 343 KB | 378 ms | 18 ms | **21.3×** | byte-identical |
| [minimal-mistakes](https://github.com/mmistakes/minimal-mistakes) | 13,540 | `58d9185` | 74 files / 225 KB | 387 ms | 21 ms | **18.6×** | byte-identical |
| [just-the-docs](https://github.com/just-the-docs/just-the-docs) | 9,079 | `2aaf003` | 35 files / 66 KB | 280 ms | 8 ms | **34.0×** | byte-identical |

Regenerate: `node bench/real-world/run.mjs all`.
