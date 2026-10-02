/** Simplified Chinese translations: accessibility (screen reader names and announcements for the file list and spreadsheets) */
export default {
  "Items": "项目",
  // Spreadsheet preview and editor: the canvas is described to screen readers
  "sheet::Sheet {name}": "工作表 {name}",
  "The cells on screen are listed in the table that follows. Scroll with the arrow keys to show other cells.": "屏幕上的单元格列在后面的表格中。用方向键滚动可显示其他单元格。",
  "sheet::Cells on screen in {name}": "{name} 中屏幕上的单元格",
  "The cells on screen are empty.": "屏幕上的单元格都是空的。",
  "Arrow keys move between cells and Shift with the arrow keys selects. Type to replace the cell's contents, or press F2 to edit them; Enter confirms and Esc cancels.": "方向键在单元格之间移动，按住 Shift 再按方向键可选择区域。直接输入会替换单元格内容，按 F2 可编辑内容；Enter 确认，Esc 取消。",
  "sheet::{range} selected": "已选择 {range}",
  "sheet::{cell}, {value}, formula {formula}": "{cell}，{value}，公式 {formula}",
  "sheet::{cell}, {value}": "{cell}，{value}",
  "sheet::{cell}, empty": "{cell}，空",
} satisfies Record<string, string>;
