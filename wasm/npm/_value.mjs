// The dart-sass `Value` type system for custom functions (the `functions`
// option), plus the wire (de)serializers that bridge it to sasso's engine.
//
// A custom function registered via `compileString(src, { functions: { ... } })`
// receives an array of `Value` objects and returns a `Value`. The engine speaks
// a compact byte protocol (see ../src/host_fn.rs); `deserializeArgs` decodes the
// engine's serialized arguments into `Value`s, and `serializeValue` encodes the
// function's returned `Value` back. The two encodings MUST stay in lockstep with
// the Rust `host_fn` (de)serializers.
//
// Supported types mirror the engine bridge: SassNumber (with units), SassString,
// SassColor (every CSS Color 4 space), SassList / SassArgumentList, SassMap,
// SassBoolean (sassTrue/sassFalse) and sassNull.

const enc = new TextEncoder();
const dec = new TextDecoder();

// ---------------- value classes ----------------

/** Base class for every Sass value. dart-sass `Value`. */
export class Value {
  get isTruthy() {
    return true;
  }
  /** `null` for `sassNull`, else the value itself (dart-sass `realNull`). */
  get realNull() {
    return this;
  }
  /** This value as a list of values (a scalar is a one-element list). */
  get asList() {
    return [this];
  }
  get hasBrackets() {
    return false;
  }
  get separator() {
    return null;
  }
  assertNumber(name) {
    throw new Error(errMsg(this, name, "a number"));
  }
  assertString(name) {
    throw new Error(errMsg(this, name, "a string"));
  }
  assertColor(name) {
    throw new Error(errMsg(this, name, "a color"));
  }
  assertMap(name) {
    throw new Error(errMsg(this, name, "a map"));
  }
  assertBoolean(name) {
    throw new Error(errMsg(this, name, "a boolean"));
  }
  /** dart-sass `Value.get(index)` — 0-based element access; scalars yield self. */
  get(index) {
    return index === 0 ? this : undefined;
  }
  equals(other) {
    return this === other;
  }
}

function errMsg(value, name, expected) {
  const what = name ? `$${name}: ` : "";
  return `${what}${value} is not ${expected}.`;
}

class SassNull extends Value {
  get isTruthy() {
    return false;
  }
  get realNull() {
    return null;
  }
  get asList() {
    return [];
  }
  toString() {
    return "null";
  }
}
export const sassNull = new SassNull();

export class SassBoolean extends Value {
  constructor(value) {
    super();
    this.value = value;
  }
  get isTruthy() {
    return this.value;
  }
  assertBoolean() {
    return this;
  }
  equals(o) {
    return o instanceof SassBoolean && o.value === this.value;
  }
  toString() {
    return String(this.value);
  }
}
export const sassTrue = new SassBoolean(true);
export const sassFalse = new SassBoolean(false);

export class SassString extends Value {
  /** `new SassString("text", { quotes: true })`. */
  constructor(text = "", options = {}) {
    super();
    this.text = String(text);
    this.hasQuotes = options.quotes !== false;
  }
  get sassLength() {
    return [...this.text].length;
  }
  assertString() {
    return this;
  }
  equals(o) {
    return o instanceof SassString && o.text === this.text;
  }
  toString() {
    return this.hasQuotes ? JSON.stringify(this.text) : this.text;
  }
}

export class SassNumber extends Value {
  /**
   * `new SassNumber(8)` or `new SassNumber(8, "px")` or
   * `new SassNumber(8, { numeratorUnits: ["px"], denominatorUnits: ["s"] })`.
   */
  constructor(value, unitOrOptions) {
    super();
    this.value = value;
    if (typeof unitOrOptions === "string") {
      this.numeratorUnits = [unitOrOptions];
      this.denominatorUnits = [];
    } else if (unitOrOptions) {
      this.numeratorUnits = unitOrOptions.numeratorUnits ?? [];
      this.denominatorUnits = unitOrOptions.denominatorUnits ?? [];
    } else {
      this.numeratorUnits = [];
      this.denominatorUnits = [];
    }
  }
  get hasUnits() {
    return this.numeratorUnits.length > 0 || this.denominatorUnits.length > 0;
  }
  get isInt() {
    return Number.isInteger(this.value) || Math.abs(this.value - Math.round(this.value)) < 1e-11;
  }
  get asInt() {
    return this.isInt ? Math.round(this.value) : null;
  }
  hasUnit(unit) {
    return this.denominatorUnits.length === 0 && this.numeratorUnits.length === 1 && this.numeratorUnits[0] === unit;
  }
  assertNumber() {
    return this;
  }
  assertInt(name) {
    const i = this.asInt;
    if (i === null) throw new Error(errMsg(this, name, "an int"));
    return i;
  }
  assertUnit(unit, name) {
    if (!this.hasUnit(unit)) throw new Error(errMsg(this, name, `a number with unit ${unit}`));
    return this;
  }
  equals(o) {
    return (
      o instanceof SassNumber &&
      o.value === this.value &&
      sameUnits(o.numeratorUnits, this.numeratorUnits) &&
      sameUnits(o.denominatorUnits, this.denominatorUnits)
    );
  }
  toString() {
    return this.value + this.numeratorUnits.map((u) => u).join("*");
  }
}

function sameUnits(a, b) {
  return a.length === b.length && a.every((u, i) => u === b[i]);
}

// CSS Color 4 channel names per space (positional order matches the engine).
const COLOR_CHANNELS = {
  rgb: ["red", "green", "blue"],
  srgb: ["red", "green", "blue"],
  "srgb-linear": ["red", "green", "blue"],
  "display-p3": ["red", "green", "blue"],
  "display-p3-linear": ["red", "green", "blue"],
  "a98-rgb": ["red", "green", "blue"],
  "prophoto-rgb": ["red", "green", "blue"],
  rec2020: ["red", "green", "blue"],
  hsl: ["hue", "saturation", "lightness"],
  hwb: ["hue", "whiteness", "blackness"],
  lab: ["lightness", "a", "b"],
  oklab: ["lightness", "a", "b"],
  lch: ["lightness", "chroma", "hue"],
  oklch: ["lightness", "chroma", "hue"],
  xyz: ["x", "y", "z"],
  "xyz-d50": ["x", "y", "z"],
  "xyz-d65": ["x", "y", "z"],
};

export class SassColor extends Value {
  /**
   * `new SassColor({ space: "oklch", lightness, chroma, hue, alpha })`, or legacy
   * `new SassColor({ red, green, blue, alpha })` (space defaults to `"rgb"`). A
   * channel may be `null` for a missing channel.
   */
  constructor(options = {}) {
    super();
    const space = options.space ?? "rgb";
    const names = COLOR_CHANNELS[space];
    if (!names) throw new Error(`sasso: unknown color space "${space}"`);
    this.space = space;
    this._channels = names.map((n) => (options[n] === undefined ? null : options[n]));
    this.alpha = options.alpha === undefined ? 1 : options.alpha;
  }
  get channelsOrNull() {
    return this._channels.slice();
  }
  get channels() {
    return this._channels.map((c) => c ?? 0);
  }
  channel(name) {
    const i = (COLOR_CHANNELS[this.space] || []).indexOf(name);
    if (i < 0) throw new Error(`sasso: color space "${this.space}" has no channel "${name}"`);
    return this._channels[i] ?? 0;
  }
  isChannelMissing(name) {
    const i = (COLOR_CHANNELS[this.space] || []).indexOf(name);
    return i >= 0 && this._channels[i] === null;
  }
  // Legacy accessors (valid for the rgb space).
  get red() {
    return this.channel("red");
  }
  get green() {
    return this.channel("green");
  }
  get blue() {
    return this.channel("blue");
  }
  assertColor() {
    return this;
  }
  equals(o) {
    return (
      o instanceof SassColor &&
      o.space === this.space &&
      o.alpha === this.alpha &&
      sameUnits(o._channels.map(String), this._channels.map(String))
    );
  }
  toString() {
    return `${this.space}(${this._channels.join(" ")} / ${this.alpha})`;
  }
}

const SEP_TO_STR = [" ", ",", "/", null];
const STR_TO_SEP = { " ": 0, ",": 1, "/": 2 };

export class SassList extends Value {
  /** `new SassList([...], { separator: ",", brackets: false })`. */
  constructor(contents = [], options = {}) {
    super();
    this._contents = [...contents];
    this._separator = options.separator === undefined ? "," : options.separator;
    this._brackets = !!options.brackets;
  }
  get asList() {
    return this._contents.slice();
  }
  get separator() {
    return this._separator;
  }
  get hasBrackets() {
    return this._brackets;
  }
  get(index) {
    return this._contents[index];
  }
  equals(o) {
    return (
      o instanceof SassList &&
      o._separator === this._separator &&
      o._brackets === this._brackets &&
      o._contents.length === this._contents.length &&
      o._contents.every((v, i) => valuesEqual(v, this._contents[i]))
    );
  }
  toString() {
    return this._contents.map(String).join(this._separator === " " ? " " : `${this._separator} `);
  }
}

/** A `$rest...` argument list: a list that also carries trailing keywords. */
export class SassArgumentList extends SassList {
  constructor(contents = [], keywords = new Map(), options = {}) {
    super(contents, options);
    this.keywords = keywords; // Map<string, Value>
  }
}

export class SassMap extends Value {
  /** `new SassMap(new Map([[key, value], ...]))` — keys are `Value`s. */
  constructor(contents = new Map()) {
    super();
    // Store as pairs for Sass value-equality lookups.
    this._pairs = [...contents.entries()];
  }
  get contents() {
    return new Map(this._pairs);
  }
  get asList() {
    return this._pairs.map(([k, v]) => new SassList([k, v], { separator: " " }));
  }
  get(key) {
    const hit = this._pairs.find(([k]) => valuesEqual(k, key));
    return hit ? hit[1] : undefined;
  }
  assertMap() {
    return this;
  }
  equals(o) {
    return (
      o instanceof SassMap &&
      o._pairs.length === this._pairs.length &&
      this._pairs.every(([k, v]) => {
        const ov = o.get(k);
        return ov !== undefined && valuesEqual(ov, v);
      })
    );
  }
  toString() {
    return `(${this._pairs.map(([k, v]) => `${k}: ${v}`).join(", ")})`;
  }
}

function valuesEqual(a, b) {
  return a === b || (a instanceof Value && a.equals(b));
}

// ---------------- wire (de)serialization (mirrors ../src/host_fn.rs) ----------------

const TAG = { NULL: 0, BOOL: 1, NUMBER: 2, STRING: 3, LIST: 4, MAP: 5, COLOR: 6 };

class Writer {
  constructor() {
    this.bytes = [];
  }
  u8(n) {
    this.bytes.push(n & 0xff);
  }
  u32(n) {
    this.bytes.push(n & 0xff, (n >>> 8) & 0xff, (n >>> 16) & 0xff, (n >>> 24) & 0xff);
  }
  f64(n) {
    const b = new Uint8Array(8);
    new DataView(b.buffer).setFloat64(0, n, true);
    for (const x of b) this.bytes.push(x);
  }
  str(s) {
    const b = enc.encode(s);
    this.u32(b.length);
    for (const x of b) this.bytes.push(x);
  }
  optF64(v) {
    if (v === null || v === undefined) {
      this.u8(0);
    } else {
      this.u8(1);
      this.f64(v);
    }
  }
  finish() {
    return new Uint8Array(this.bytes);
  }
}

function writeValue(w, v) {
  if (v === null || v === undefined || v instanceof SassNull) {
    w.u8(TAG.NULL);
  } else if (v instanceof SassBoolean) {
    w.u8(TAG.BOOL);
    w.u8(v.value ? 1 : 0);
  } else if (v instanceof SassNumber) {
    w.u8(TAG.NUMBER);
    w.f64(v.value);
    w.u32(v.numeratorUnits.length);
    for (const u of v.numeratorUnits) w.str(u);
    w.u32(v.denominatorUnits.length);
    for (const u of v.denominatorUnits) w.str(u);
  } else if (v instanceof SassString) {
    w.u8(TAG.STRING);
    w.u8(v.hasQuotes ? 1 : 0);
    w.str(v.text);
  } else if (v instanceof SassColor) {
    w.u8(TAG.COLOR);
    w.str(v.space);
    for (const c of v._channels) w.optF64(c);
    w.optF64(v.alpha);
  } else if (v instanceof SassList) {
    w.u8(TAG.LIST);
    w.u8(v._separator === null ? 3 : (STR_TO_SEP[v._separator] ?? 1));
    w.u8(v._brackets ? 1 : 0);
    w.u32(v._contents.length);
    for (const it of v._contents) writeValue(w, it);
    const kw = v instanceof SassArgumentList ? v.keywords : null;
    if (kw && kw.size) {
      w.u8(1);
      w.u32(kw.size);
      for (const [k, val] of kw) {
        writeValue(w, new SassString(k, { quotes: false }));
        writeValue(w, val);
      }
    } else {
      w.u8(0);
    }
  } else if (v instanceof SassMap) {
    w.u8(TAG.MAP);
    w.u32(v._pairs.length);
    for (const [k, val] of v._pairs) {
      writeValue(w, k);
      writeValue(w, val);
    }
  } else {
    throw new Error(`sasso: a custom function returned a value sasso can't represent (${v})`);
  }
}

/** Serialize a single returned `Value` to the engine's wire format. */
export function serializeValue(v) {
  const w = new Writer();
  writeValue(w, v);
  return w.finish();
}

class Reader {
  constructor(buf) {
    this.view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    this.bytes = buf;
    this.pos = 0;
  }
  u8() {
    return this.bytes[this.pos++];
  }
  u32() {
    const n = this.view.getUint32(this.pos, true);
    this.pos += 4;
    return n;
  }
  f64() {
    const n = this.view.getFloat64(this.pos, true);
    this.pos += 8;
    return n;
  }
  str() {
    const len = this.u32();
    const s = dec.decode(this.bytes.subarray(this.pos, this.pos + len));
    this.pos += len;
    return s;
  }
  optF64() {
    return this.u8() ? this.f64() : null;
  }
}

function readValue(r) {
  const tag = r.u8();
  switch (tag) {
    case TAG.NULL:
      return sassNull;
    case TAG.BOOL:
      return r.u8() ? sassTrue : sassFalse;
    case TAG.NUMBER: {
      const value = r.f64();
      const numeratorUnits = [];
      for (let n = r.u32(); n > 0; n--) numeratorUnits.push(r.str());
      const denominatorUnits = [];
      for (let n = r.u32(); n > 0; n--) denominatorUnits.push(r.str());
      return new SassNumber(value, { numeratorUnits, denominatorUnits });
    }
    case TAG.STRING: {
      const quotes = r.u8() !== 0;
      return new SassString(r.str(), { quotes });
    }
    case TAG.COLOR: {
      const space = r.str();
      const names = COLOR_CHANNELS[space] || ["c0", "c1", "c2"];
      const opts = { space };
      for (let i = 0; i < 3; i++) opts[names[i]] = r.optF64();
      opts.alpha = r.optF64();
      return new SassColor(opts);
    }
    case TAG.LIST: {
      const sep = SEP_TO_STR[r.u8()];
      const brackets = r.u8() !== 0;
      const n = r.u32();
      const items = [];
      for (let i = 0; i < n; i++) items.push(readValue(r));
      let keywords = null;
      if (r.u8() !== 0) {
        keywords = new Map();
        for (let k = r.u32(); k > 0; k--) {
          const key = readValue(r);
          const val = readValue(r);
          keywords.set(key instanceof SassString ? key.text : String(key), val);
        }
      }
      const options = { separator: sep, brackets };
      return keywords ? new SassArgumentList(items, keywords, options) : new SassList(items, options);
    }
    case TAG.MAP: {
      const n = r.u32();
      const m = new Map();
      for (let i = 0; i < n; i++) {
        const k = readValue(r);
        const v = readValue(r);
        m.set(k, v);
      }
      return new SassMap(m);
    }
    default:
      throw new Error(`sasso: unknown value tag ${tag} from the engine`);
  }
}

/** Decode the engine's serialized argument list (`u32` count + values). */
export function deserializeArgs(buf) {
  const r = new Reader(buf);
  const n = r.u32();
  const args = [];
  for (let i = 0; i < n; i++) args.push(readValue(r));
  return args;
}

/** The public `Value` API, for re-export on the package's named + default exports. */
export const valueApi = {
  Value,
  SassBoolean,
  SassColor,
  SassList,
  SassArgumentList,
  SassMap,
  SassNumber,
  SassString,
  sassTrue,
  sassFalse,
  sassNull,
};
