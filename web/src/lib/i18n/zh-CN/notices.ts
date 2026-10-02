/** Simplified Chinese translations: spreadsheet preview, restore notices, storage location and third-party sign-in validation messages, screen reader names */
export default {
  "Restored": "已復原",
  "The file's content is too large to preview. Download it and open it in Office.": "檔案內容過大，無法預覽。請下載後用 Office 開啟。",
  "Laying out this document took too long, so the preview was stopped. Download it and open it in Office.": "文件排版時間過長，已停止預覽。請下載後用 Office 開啟。",
  "The file is too large (over {size}) to preview online. Download it to open it.": "檔案太大（超過 {size}），無法線上預覽，請下載後開啟。",
  "The file's content is too large to open. Download it and open it in Excel.": "檔案內容過大，無法開啟。請下載後用 Excel 開啟。",
  // Server messages: unencrypted or certificate-unverified connections are only allowed to private network addresses
  "Unencrypted FTP": "未加密的 FTP",
  "Skipping certificate verification": "略過憑證驗證",
  "{reason} requires a host address": "{reason}需要填寫主機位址",
  "Invalid tenant: enter a tenant ID (GUID) or domain, e.g. contoso.onmicrosoft.com": "租用戶格式不正確：請填寫租用戶 ID（GUID）或網域，例如 contoso.onmicrosoft.com",
  "Show more": "顯示更多",
  // Screen reader names: skip link, progress bars, share page heading
  "Skip to main content": "跳到主要內容",
  "Upload progress": "上傳進度",
  "Download progress": "下載進度",
  "Storage used": "已使用空間",
  "Share link": "分享連結",
  "Charts and pictures in this workbook couldn't be shown. Reload the page.": "無法顯示此活頁簿中的圖表和圖片，請重新整理頁面。",
  "light mode": "淺色模式",
  "dark mode": "深色模式",
  "In {mode}, the accent color stands out too little from the page ({ratio}:1, at least 3:1 is needed): focus rings and selected items are hard to see.": "{mode}下，強調色與頁面的對比太低（{ratio}:1，至少需要 3:1）：焦點框和選取的項目不易看清。",
  "In {mode}, button text on the accent color is hard to read ({ratio}:1, at least 4.5:1 is needed).": "{mode}下，強調色上的按鈕文字不易閱讀（{ratio}:1，至少需要 4.5:1）。",
} satisfies Record<string, string>;
