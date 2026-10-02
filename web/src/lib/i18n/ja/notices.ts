/** Japanese translations: spreadsheet preview, restore notices, storage location and third-party sign-in validation messages, screen reader names */
export default {
  "Restored": "復元しました",
  "The file's content is too large to preview. Download it and open it in Office.": "ファイルの内容が大きすぎるため、プレビューできません。ダウンロードして Office で開いてください。",
  "Laying out this document took too long, so the preview was stopped. Download it and open it in Office.": "文書のレイアウトに時間がかかりすぎたため、プレビューを停止しました。ダウンロードして Office で開いてください。",
  "The file is too large (over {size}) to preview online. Download it to open it.": "ファイルが大きすぎる（{size} 超）ため、オンラインでプレビューできません。ダウンロードして開いてください。",
  "The file's content is too large to open. Download it and open it in Excel.": "ファイルの内容が大きすぎるため、開けません。ダウンロードして Excel で開いてください。",
  // Server messages: unencrypted or certificate-unverified connections are only allowed to private network addresses
  "Unencrypted FTP": "暗号化されていない FTP",
  "Skipping certificate verification": "証明書の検証のスキップ",
  "{reason} requires a host address": "{reason} にはホストのアドレスの入力が必要です",
  "Invalid tenant: enter a tenant ID (GUID) or domain, e.g. contoso.onmicrosoft.com": "テナントが無効です：テナント ID（GUID）またはドメインを入力してください（例：contoso.onmicrosoft.com）",
  "Show more": "さらに表示",
  // Screen reader names: skip link, progress bars, share page heading
  "Skip to main content": "メインコンテンツにスキップ",
  "Upload progress": "アップロードの進行状況",
  "Download progress": "ダウンロードの進行状況",
  "Storage used": "使用済み容量",
  "Share link": "共有リンク",
  "Charts and pictures in this workbook couldn't be shown. Reload the page.": "このブックのグラフと画像を表示できませんでした。ページを再読み込みしてください。",
  "light mode": "ライトモード",
  "dark mode": "ダークモード",
  "In {mode}, the accent color stands out too little from the page ({ratio}:1, at least 3:1 is needed): focus rings and selected items are hard to see.": "{mode} では、アクセントカラーとページのコントラストが低すぎます（{ratio}:1、3:1 以上が必要）。フォーカスの枠や選択した項目が見えにくくなります。",
  "In {mode}, button text on the accent color is hard to read ({ratio}:1, at least 4.5:1 is needed).": "{mode} では、アクセントカラー上のボタンの文字が読みにくくなっています（{ratio}:1、4.5:1 以上が必要）。",
} satisfies Record<string, string>;
