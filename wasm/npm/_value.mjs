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
// Collection accessors (`asList`, `channels`, `numeratorUnits`,
// `SassMap.contents`, …) return the immutable `List`/`OrderedMap` from
// `./_immutable.mjs`, matching dart-sass so a function written for `sass` runs
// unchanged.
//
// Supported types: SassNumber (with units), SassString, SassColor (every CSS
// Color 4 space), SassList / SassArgumentList, SassMap, SassBoolean
// (sassTrue/sassFalse) and sassNull. (calc() and first-class function/mixin refs
// are not yet representable — see ../../docs/CUSTOM_FUNCTIONS_COMPAT.md.)

import { List, OrderedMap, list, immutableIs } from "./_immutable.mjs";

const enc = new TextEncoder();
const dec = new TextDecoder();

// Engine-routed value operations: methods that need unit / color-space math
// (e.g. SassNumber.convert, SassColor.toSpace) call back into the wasm engine so
// the conversions reuse the exact Rust math. The loader injects `engine` (an
// independent value instance, so these work standalone and re-entrantly).
let engine = null;
/** Injected by the loader: `(op, argsBytes) => resultBytes` (throws on error). */
export function setEngine(fn) {
  engine = fn;
}
const OP_NUMBER_CONVERT = 1;
const OP_NUMBER_COMPATIBLE = 2;

function runOp(op, args) {
  if (!engine) throw new Error("sasso: the value engine is not initialized (import the package first)");
  const w = new Writer();
  w.u32(args.length);
  for (const a of args) writeValue(w, a);
  return readValue(new Reader(engine(op, w.finish())));
}

/** A space-separated SassList of unquoted unit-name strings, for a convert op. */
function unitList(units) {
  return new SassList(toArray(units).map((u) => new SassString(String(u), { quotes: false })), { separator: " " });
}

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
  /** This value as an immutable list (a scalar is a one-element list). */
  get asList() {
    return list([this]);
  }
  get hasBrackets() {
    return false;
  }
  get separator() {
    return null;
  }
  /** dart-sass `Value.get(index)` — 0-based, supports negative indexing. */
  get(index) {
    return this.asList.get(index);
  }
  /** Convert a 1-based Sass index (negatives count from the end) to 0-based. */
  sassIndexToListIndex(sassIndex, name) {
    const index = sassIndex.assertNumber(name).assertInt(name);
    if (index === 0) throw new Error(prefix(name) + "List index may not be 0.");
    const size = this.asList.size;
    if (Math.abs(index) > size) {
      throw new Error(prefix(name) + `Invalid index ${index} for a list with ${size} elements.`);
    }
    return index < 0 ? size + index : index - 1;
  }
  /** This value as a map if it is one (or the empty list `()`), else `null`. */
  tryMap() {
    return null;
  }
  assertNumber(name) {
    throw new Error(notA(this, name, "a number"));
  }
  assertString(name) {
    throw new Error(notA(this, name, "a string"));
  }
  assertColor(name) {
    throw new Error(notA(this, name, "a color"));
  }
  assertMap(name) {
    throw new Error(notA(this, name, "a map"));
  }
  assertBoolean(name) {
    throw new Error(notA(this, name, "a boolean"));
  }
  assertCalculation(name) {
    throw new Error(notA(this, name, "a calculation"));
  }
  assertFunction(name) {
    throw new Error(notA(this, name, "a function reference"));
  }
  assertMixin(name) {
    throw new Error(notA(this, name, "a mixin reference"));
  }
  equals(other) {
    return this === other;
  }
  hashCode() {
    return 0;
  }
}

function prefix(name) {
  return name ? `$${name}: ` : "";
}
function notA(value, name, expected) {
  return `${prefix(name)}${value} is not ${expected}.`;
}
function hashStr(s) {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (Math.imul(31, h) + s.charCodeAt(i)) | 0;
  return h;
}

class SassNull extends Value {
  get isTruthy() {
    return false;
  }
  get realNull() {
    return null;
  }
  get asList() {
    return list([]);
  }
  hashCode() {
    return 0;
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
  hashCode() {
    return this.value ? 1 : 2;
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
  static empty(options = {}) {
    return new SassString("", options);
  }
  get sassLength() {
    return [...this.text].length;
  }
  /** Convert a 1-based Sass string index (by code point) to a 0-based one. */
  sassIndexToStringIndex(sassIndex, name) {
    const index = sassIndex.assertNumber(name).assertInt(name);
    if (index === 0) throw new Error(prefix(name) + "String index may not be 0.");
    const len = this.sassLength;
    if (Math.abs(index) > len) {
      throw new Error(prefix(name) + `Invalid index ${index} for a string with ${len} characters.`);
    }
    return index < 0 ? len + index : index - 1;
  }
  assertString() {
    return this;
  }
  equals(o) {
    return o instanceof SassString && o.text === this.text;
  }
  hashCode() {
    return hashStr(this.text);
  }
  toString() {
    return this.hasQuotes ? JSON.stringify(this.text) : this.text;
  }
}

export class SassNumber extends Value {
  /**
   * `new SassNumber(8)`, `new SassNumber(8, "px")`, or
   * `new SassNumber(8, { numeratorUnits: ["px"], denominatorUnits: ["s"] })`.
   */
  constructor(value, unitOrOptions) {
    super();
    this.value = value;
    if (typeof unitOrOptions === "string") {
      this._numeratorUnits = [unitOrOptions];
      this._denominatorUnits = [];
    } else if (unitOrOptions) {
      this._numeratorUnits = toArray(unitOrOptions.numeratorUnits);
      this._denominatorUnits = toArray(unitOrOptions.denominatorUnits);
    } else {
      this._numeratorUnits = [];
      this._denominatorUnits = [];
    }
  }
  get numeratorUnits() {
    return list(this._numeratorUnits);
  }
  get denominatorUnits() {
    return list(this._denominatorUnits);
  }
  get hasUnits() {
    return this._numeratorUnits.length > 0 || this._denominatorUnits.length > 0;
  }
  get isInt() {
    return Number.isInteger(this.value) || Math.abs(this.value - Math.round(this.value)) < 1e-11;
  }
  get asInt() {
    return this.isInt ? Math.round(this.value) : null;
  }
  hasUnit(unit) {
    return this._denominatorUnits.length === 0 && this._numeratorUnits.length === 1 && this._numeratorUnits[0] === unit;
  }
  assertNumber() {
    return this;
  }
  assertInt(name) {
    const i = this.asInt;
    if (i === null) throw new Error(notA(this, name, "an int"));
    return i;
  }
  assertNoUnits(name) {
    if (this.hasUnits) throw new Error(notA(this, name, "unitless"));
    return this;
  }
  assertUnit(unit, name) {
    if (!this.hasUnit(unit)) throw new Error(notA(this, name, `a number with unit ${unit}`));
    return this;
  }
  assertInRange(min, max, name) {
    if (this.value < min - 1e-11 || this.value > max + 1e-11) {
      throw new Error(prefix(name) + `Expected ${this} to be within ${min} and ${max}.`);
    }
    return this.value < min ? min : this.value > max ? max : this.value;
  }
  // --- unit conversion (routed to the engine's Rust math) ---
  compatibleWithUnit(unit) {
    return runOp(OP_NUMBER_COMPATIBLE, [this, new SassString(unit, { quotes: false })]).value;
  }
  convert(newNumerators, newDenominators, name) {
    const r = runOp(OP_NUMBER_CONVERT, [this, unitList(newNumerators), unitList(newDenominators), sassFalse]);
    if (name && r instanceof SassNumber === false) throw new Error(prefix(name) + "conversion failed");
    return r;
  }
  coerce(newNumerators, newDenominators) {
    return runOp(OP_NUMBER_CONVERT, [this, unitList(newNumerators), unitList(newDenominators), sassTrue]);
  }
  convertToMatch(other) {
    return this.convert(other.numeratorUnits.toArray(), other.denominatorUnits.toArray());
  }
  coerceToMatch(other) {
    return this.coerce(other.numeratorUnits.toArray(), other.denominatorUnits.toArray());
  }
  convertValue(newNumerators, newDenominators) {
    return this.convert(newNumerators, newDenominators).value;
  }
  convertValueToMatch(other) {
    return this.convertToMatch(other).value;
  }
  coerceValue(newNumerators, newDenominators) {
    return this.coerce(newNumerators, newDenominators).value;
  }
  coerceValueToMatch(other) {
    return this.coerceToMatch(other).value;
  }
  equals(o) {
    return (
      o instanceof SassNumber &&
      o.value === this.value &&
      sameUnits(o._numeratorUnits, this._numeratorUnits) &&
      sameUnits(o._denominatorUnits, this._denominatorUnits)
    );
  }
  hashCode() {
    return (this.value | 0) ^ hashStr(this._numeratorUnits.join("*"));
  }
  toString() {
    return this.value + this._numeratorUnits.join("*");
  }
}

function toArray(x) {
  if (!x) return [];
  return Array.isArray(x) ? x.slice() : typeof x.toArray === "function" ? x.toArray() : [...x];
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
const LEGACY_SPACES = new Set(["rgb", "hsl", "hwb"]);

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
  get isLegacy() {
    return LEGACY_SPACES.has(this.space);
  }
  get channelsOrNull() {
    return list(this._channels);
  }
  get channels() {
    return list(this._channels.map((c) => c ?? 0));
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
  // Legacy accessors (valid for the rgb space; cross-space conversion is a
  // Tier-2 item — see docs/CUSTOM_FUNCTIONS_COMPAT.md).
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
  hashCode() {
    return hashStr(this.space) ^ ((this.alpha * 1000) | 0);
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
    this._contents = toArray(contents);
    this._separator = options.separator === undefined ? "," : options.separator;
    this._brackets = !!options.brackets;
  }
  get asList() {
    return list(this._contents);
  }
  get separator() {
    return this._separator;
  }
  get hasBrackets() {
    return this._brackets;
  }
  tryMap() {
    return this._contents.length === 0 ? new SassMap() : null;
  }
  assertMap(name) {
    if (this._contents.length === 0) return new SassMap();
    throw new Error(notA(this, name, "a map"));
  }
  equals(o) {
    return (
      o instanceof SassList &&
      o._separator === this._separator &&
      o._brackets === this._brackets &&
      o._contents.length === this._contents.length &&
      o._contents.every((v, i) => immutableIs(v, this._contents[i]))
    );
  }
  hashCode() {
    return this._contents.reduce((h, v) => (Math.imul(31, h) + (v.hashCode?.() ?? 0)) | 0, 7);
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
    if (contents instanceof OrderedMap) this._pairs = contents.toArray();
    else this._pairs = [...contents].map(([k, v]) => [k, v]);
  }
  static empty() {
    return new SassMap();
  }
  /** The map's contents as an immutable, value-keyed `OrderedMap`. */
  get contents() {
    return new OrderedMap(this._pairs);
  }
  get asList() {
    return list(this._pairs.map(([k, v]) => new SassList([k, v], { separator: " " })));
  }
  get separator() {
    return ",";
  }
  tryMap() {
    return this;
  }
  assertMap() {
    return this;
  }
  equals(o) {
    return (
      o instanceof SassMap &&
      o._pairs.length === this._pairs.length &&
      this._pairs.every(([k, v]) => {
        const ov = o.contents.get(k);
        return ov !== undefined && immutableIs(ov, v);
      })
    );
  }
  hashCode() {
    return this._pairs.reduce((h, [k, v]) => (h + ((k.hashCode?.() ?? 0) ^ (v.hashCode?.() ?? 0))) | 0, 11);
  }
  toString() {
    return `(${this._pairs.map(([k, v]) => `${k}: ${v}`).join(", ")})`;
  }
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
    w.u32(v._numeratorUnits.length);
    for (const u of v._numeratorUnits) w.str(u);
    w.u32(v._denominatorUnits.length);
    for (const u of v._denominatorUnits) w.str(u);
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
  List,
  OrderedMap,
};
