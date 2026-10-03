/**
 * The Mac style's own assets: its look (mac.css), its icons for items (documents.tsx) and its symbols for the sidebar and
 * toolbar (symbols.ts). A chunk of its own, loaded only while the Mac style is in use (../loadArt.ts).
 */
import "./mac.css";

export { MacItemIcon } from "./documents";
export { MAC_SYMBOLS, type MacSymbol } from "./symbols";
