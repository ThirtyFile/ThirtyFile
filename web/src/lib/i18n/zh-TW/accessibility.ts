/** Traditional Chinese translations: accessibility (screen reader names and announcements for the file list and spreadsheets) */
export default {
  "Items": "項目",
  // Spreadsheet preview and editor: the canvas is described to screen readers
  "sheet::Sheet {name}": "工作表 {name}",
  "The cells on screen are listed in the table that follows. Scroll with the arrow keys to show other cells.": "畫面上的儲存格列在後面的表格中。用方向鍵捲動可顯示其他儲存格。",
  "sheet::Cells on screen in {name}": "{name} 畫面上的儲存格",
  "The cells on screen are empty.": "畫面上的儲存格都是空白的。",
  "Arrow keys move between cells and Shift with the arrow keys selects. Type to replace the cell's contents, or press F2 to edit them; Enter confirms and Esc cancels.": "方向鍵在儲存格之間移動，按住 Shift 再按方向鍵可選取範圍。直接輸入會取代儲存格內容，按 F2 可編輯內容；Enter 確認，Esc 取消。",
  "sheet::{range} selected": "已選取 {range}",
  "sheet::{cell}, {value}, formula {formula}": "{cell}，{value}，公式 {formula}",
  "sheet::{cell}, {value}": "{cell}，{value}",
  "sheet::{cell}, empty": "{cell}，空白",
} satisfies Record<string, string>;
