/**
 * CSS for the preview (a single <style> element; class names all start with tf-pptx-).
 */

export const C = {
  deck: "tf-pptx-deck",
  frame: "tf-pptx-frame",
  slide: "tf-pptx-slide",
  hidden: "tf-pptx-hidden",
  sp: "tf-pptx-sp",
  svg: "tf-pptx-svg",
  tx: "tf-pptx-tx",
  tb: "tf-pptx-tb",
  nowrap: "tf-pptx-nw",
  p: "tf-pptx-p",
  bullet: "tf-pptx-bu",
  link: "tf-pptx-a",
  pic: "tf-pptx-pic",
  na: "tf-pptx-na",
  table: "tf-pptx-tbl",
  bg: "tf-pptx-bg",
  drawing: "tf-pptx-dw",
} as const;

export const CSS = `
.${C.deck}{display:flex;flex-direction:column;align-items:center;gap:20px;padding:20px 0;box-sizing:border-box;width:100%}
.${C.frame}{position:relative;flex:none;overflow:hidden;box-shadow:0 1px 3px rgba(0,0,0,.18),0 4px 14px rgba(0,0,0,.12);background:#fff}
.${C.slide}{position:absolute;left:0;top:0;transform-origin:0 0;overflow:hidden;background:#fff;color:#000;
  font-kerning:normal;text-rendering:optimizeLegibility;-webkit-font-smoothing:antialiased;line-height:1.2}
.${C.hidden}>.${C.slide}{opacity:.45}
.${C.bg}{position:absolute;inset:0;overflow:hidden}
.${C.sp}{position:absolute;box-sizing:border-box}
.${C.svg}{position:absolute;left:0;top:0;overflow:visible;pointer-events:none}
.${C.tx}{position:absolute;box-sizing:border-box;display:flex;flex-direction:column;overflow:visible}
.${C.tb}{flex:none;min-width:0}
.${C.nowrap}>.${C.p}{white-space:pre}
.${C.p}{margin:0;white-space:pre-wrap;overflow-wrap:break-word;tab-size:96px;-moz-tab-size:96px}
.${C.drawing}{line-height:1.2;text-align:left;letter-spacing:normal;text-transform:none;font-style:normal;font-weight:normal;color:#000;text-indent:0}
.${C.drawing} svg,.${C.drawing} img{max-width:none}
.${C.bullet} img{display:inline-block}
.${C.bullet}{display:inline-block;text-indent:0;white-space:pre;font-style:normal;font-weight:normal;text-decoration:none}
.${C.link}{color:inherit;text-decoration:none;cursor:pointer}
.${C.pic}{position:absolute;overflow:hidden}
.${C.pic}>img{position:absolute;max-width:none;display:block;user-select:none}
.${C.na}{position:absolute;inset:0;background:#eceff1;border:1px solid #cfd8dc;box-sizing:border-box}
.${C.table}{position:absolute;border-collapse:collapse;table-layout:fixed;border-spacing:0}
.${C.table} td{padding:0;box-sizing:border-box;overflow-wrap:break-word}
`;
