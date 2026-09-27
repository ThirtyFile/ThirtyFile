/**
 * Value axis tick computation (auto-rounded min, max and major unit; logarithmic scale) and pixel mapping.
 */

export interface ScaleInput {
  /** Data min and max */
  lo: number;
  hi: number;
  min: number | null;
  max: number | null;
  major: number | null;
  minor: number | null;
  logBase: number | null;
  /** Percent stacked: fixed at 0–100% */
  percent?: boolean;
  /** Don't start at 0 automatically (e.g. scatter X axes follow the same rule; parameter kept for special charts) */
  noZero?: boolean;
}

export interface ValueScale {
  min: number;
  max: number;
  major: number;
  minor: number;
  log: number | null;
  ticks: number[];
  minorTicks: number[];
}

/** Candidate intervals of 1, 2, 5 × 10^k (ascending) */
function candidates(range: number): number[] {
  const out: number[] = [];
  const e0 = Math.floor(Math.log10(Math.max(range, 1e-300))) - 3;
  for (let e = e0; e <= e0 + 6; e++) for (const m of [1, 2, 5]) out.push(m * Math.pow(10, e));
  return out;
}

/** Remove floating-point error (round to the interval's number of decimals) */
export function clean(v: number, step: number): number {
  const dec = Math.max(0, -Math.floor(Math.log10(Math.abs(step) || 1)) + 2);
  return Number(v.toFixed(Math.min(dec, 15)));
}

function tickList(min: number, max: number, step: number): number[] {
  const out: number[] = [];
  if (!(step > 0) || !Number.isFinite(min) || !Number.isFinite(max)) return out;
  const n = Math.round((max - min) / step);
  if (n > 1000) return [min, max];
  for (let i = 0; i <= n + 1e-9; i++) {
    const v = clean(min + i * step, step);
    if (v <= max + step * 1e-9) out.push(v);
  }
  return out;
}

/**
 * Automatic ticks:
 * - Start at 0 when all values are positive and the min is below 5/6 of the max (negatives handled symmetrically)
 * - Max plus 5% of the range, rounded up to the major unit
 * - Major unit is the smallest of 1/2/5 × 10^k whose interval count does not exceed maxIntervals
 */
export function valueScale(inp: ScaleInput, maxIntervals: number): ValueScale {
  if (inp.logBase && inp.logBase > 1) return logScale(inp);
  let lo = inp.lo;
  let hi = inp.hi;
  if (!Number.isFinite(lo) || !Number.isFinite(hi)) {
    lo = 0;
    hi = 1;
  }
  if (inp.percent) {
    lo = lo < 0 ? -1 : 0;
    hi = hi > 0 || lo === 0 ? 1 : 0;
  }
  if (inp.min !== null) lo = Math.min(lo, inp.min);
  if (inp.max !== null) hi = Math.max(hi, inp.max);
  if (lo === hi) {
    if (lo === 0) hi = 1;
    else if (lo > 0) lo = 0;
    else hi = 0;
  }
  // Origin: start at 0 when data is all on one side and the gap is large enough
  let autoLo = lo;
  let autoHi = hi;
  if (!inp.noZero && !inp.percent) {
    if (lo >= 0 && lo < (hi * 5) / 6) autoLo = 0;
    if (hi <= 0 && hi > (lo * 5) / 6) autoHi = 0;
  }
  const range = autoHi - autoLo || Math.abs(autoHi) || 1;
  const fixedMin = inp.min;
  const fixedMax = inp.max;
  const pick = (step: number) => {
    let mn: number;
    if (fixedMin !== null) mn = fixedMin;
    else if (inp.percent || autoLo === 0) mn = autoLo;
    else if (autoLo > 0) mn = Math.floor((autoLo - (autoHi - autoLo) / 2) / step) * step;
    else mn = Math.floor((autoLo - 0.05 * range) / step) * step;
    if (fixedMin === null && autoLo >= 0 && mn < 0) mn = 0;
    let mx: number;
    if (fixedMax !== null) mx = fixedMax;
    else if (inp.percent || autoHi === 0) mx = autoHi;
    else mx = Math.ceil((autoHi + 0.05 * range) / step - 1e-9) * step;
    if (fixedMax === null && autoHi <= 0 && mx > 0) mx = 0;
    return { mn: clean(mn, step), mx: clean(mx, step) };
  };
  let step = inp.major && inp.major > 0 ? inp.major : 0;
  let bounds = { mn: autoLo, mx: autoHi };
  if (step) bounds = pick(step);
  else {
    const list = candidates(range);
    step = list[list.length - 1];
    for (const c of list) {
      const b = pick(c);
      if ((b.mx - b.mn) / c <= Math.max(1, maxIntervals) + 1e-9) {
        step = c;
        bounds = b;
        break;
      }
    }
    if (bounds.mn === autoLo && bounds.mx === autoHi) bounds = pick(step);
  }
  if (bounds.mx <= bounds.mn) bounds.mx = bounds.mn + step;
  const minor = inp.minor && inp.minor > 0 ? inp.minor : step / 5;
  return {
    min: bounds.mn,
    max: bounds.mx,
    major: step,
    minor,
    log: null,
    ticks: tickList(bounds.mn, bounds.mx, step),
    minorTicks: tickList(bounds.mn, bounds.mx, minor),
  };
}

function logScale(inp: ScaleInput): ValueScale {
  const base = inp.logBase!;
  const lg = (v: number) => Math.log(v) / Math.log(base);
  const lo = inp.lo > 0 ? inp.lo : 1;
  const hi = inp.hi > 0 ? inp.hi : lo * base;
  const min = inp.min && inp.min > 0 ? inp.min : Math.pow(base, Math.floor(lg(lo) + 1e-9));
  let max = inp.max && inp.max > 0 ? inp.max : Math.pow(base, Math.ceil(lg(hi) - 1e-9));
  if (max <= min) max = min * base;
  const ticks: number[] = [];
  const minorTicks: number[] = [];
  const e0 = Math.floor(lg(min) + 1e-9);
  const e1 = Math.ceil(lg(max) - 1e-9);
  // Cap the number of ticks (files can specify extreme bases or ranges)
  const stepE = Math.max(inp.major && inp.major > 1 ? Math.max(1, Math.round(lg(inp.major))) : 1, Math.ceil((e1 - e0) / 1000));
  for (let e = e0; e <= e1 && ticks.length < 1000; e += stepE) {
    const v = Math.pow(base, e);
    if (v >= min * (1 - 1e-9) && v <= max * (1 + 1e-9)) ticks.push(Number(v.toPrecision(12)));
    for (let k = 2; k < base && minorTicks.length < 5000; k++) {
      const m = v * k;
      if (m > min && m < max) minorTicks.push(m);
    }
  }
  return { min, max, major: base, minor: base, log: base, ticks, minorTicks };
}

/** Value → proportional position in 0–1 */
export function fraction(sc: ValueScale, v: number): number {
  if (sc.log) {
    if (v <= 0) return 0;
    const lg = (x: number) => Math.log(x) / Math.log(sc.log!);
    return (lg(v) - lg(sc.min)) / (lg(sc.max) - lg(sc.min) || 1);
  }
  return (v - sc.min) / (sc.max - sc.min || 1);
}

/** Divisor for display units (dispUnits) */
export const DISP_UNITS: Record<string, number> = {
  hundreds: 1e2,
  thousands: 1e3,
  tenThousands: 1e4,
  hundredThousands: 1e5,
  millions: 1e6,
  tenMillions: 1e7,
  hundredMillions: 1e8,
  billions: 1e9,
  trillions: 1e12,
};
