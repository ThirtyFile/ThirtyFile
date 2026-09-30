/** Why an Office file couldn't be read or written; the code says what went wrong, and the app says it in words */
export type OoxmlErrorCode =
  /** Not a ZIP package: damaged, or not made by Office */
  | "not-ooxml"
  /** The content is too large to open */
  | "too-large"
  /** A spreadsheet without its workbook part */
  | "no-workbook"
  /** A workbook without worksheets that can be edited */
  | "no-sheets"
  /** A worksheet whose structure isn't recognized */
  | "unknown-sheet"
  /** A cell reference that doesn't name a cell (`detail`: the reference) */
  | "bad-cell-reference";

export class OoxmlError extends Error {
  constructor(
    public code: OoxmlErrorCode,
    /** What the error is about, such as the reference that isn't valid */
    public detail?: string,
  ) {
    super(detail === undefined ? code : `${code}: ${detail}`);
  }
}
