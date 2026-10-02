/** Japanese translations: accessibility (screen reader names and announcements for the file list and spreadsheets) */
export default {
  "Items": "Items",
  // Spreadsheet preview and editor: the canvas is described to screen readers
  "sheet::Sheet {name}": "Sheet {name}",
  "The cells on screen are listed in the table that follows. Scroll with the arrow keys to show other cells.": "The cells on screen are listed in the table that follows. Scroll with the arrow keys to show other cells.",
  "sheet::Cells on screen in {name}": "Cells on screen in {name}",
  "The cells on screen are empty.": "The cells on screen are empty.",
  "Arrow keys move between cells and Shift with the arrow keys selects. Type to replace the cell's contents, or press F2 to edit them; Enter confirms and Esc cancels.": "Arrow keys move between cells and Shift with the arrow keys selects. Type to replace the cell's contents, or press F2 to edit them; Enter confirms and Esc cancels.",
  "sheet::{range} selected": "{range} selected",
  "sheet::{cell}, {value}, formula {formula}": "{cell}, {value}, formula {formula}",
  "sheet::{cell}, {value}": "{cell}, {value}",
  "sheet::{cell}, empty": "{cell}, empty",
} satisfies Record<string, string>;
