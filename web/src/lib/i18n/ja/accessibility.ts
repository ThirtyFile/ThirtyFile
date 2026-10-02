/** Japanese translations: accessibility (screen reader names and announcements for the file list and spreadsheets) */
export default {
  "Items": "項目",
  // Spreadsheet preview and editor: the canvas is described to screen readers
  "sheet::Sheet {name}": "シート {name}",
  "The cells on screen are listed in the table that follows. Scroll with the arrow keys to show other cells.": "画面上のセルは、続く表に一覧表示されます。方向キーでスクロールすると、他のセルが表示されます。",
  "sheet::Cells on screen in {name}": "{name} の画面上のセル",
  "The cells on screen are empty.": "画面上のセルはすべて空です。",
  "Arrow keys move between cells and Shift with the arrow keys selects. Type to replace the cell's contents, or press F2 to edit them; Enter confirms and Esc cancels.": "方向キーでセル間を移動し、Shift キーを押しながら方向キーで範囲を選択します。入力するとセルの内容が置き換わり、F2 キーで内容を編集できます。Enter キーで確定、Esc キーでキャンセルします。",
  "sheet::{range} selected": "{range} を選択中",
  "sheet::{cell}, {value}, formula {formula}": "{cell}、{value}、数式 {formula}",
  "sheet::{cell}, {value}": "{cell}、{value}",
  "sheet::{cell}, empty": "{cell}、空白",
} satisfies Record<string, string>;
