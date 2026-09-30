/**
 * Stylesheet for the Word preview (all classes are prefixed with tf-docx-).
 */

export const DOCX_CSS = `
.tf-docx{display:flex;flex-direction:column;align-items:center;gap:16px;padding:16px 0;color:#000;background:transparent;text-autospace:ideograph-alpha ideograph-numeric;-webkit-text-size-adjust:100%;font-kerning:normal}
.tf-docx *,.tf-docx *::before,.tf-docx *::after{box-sizing:border-box}
.tf-docx p,.tf-docx table,.tf-docx td{margin:0}
.tf-docx-wrap{position:relative;flex:none}
.tf-docx-page{position:absolute;left:0;top:0;transform-origin:0 0;background:#fff;box-shadow:0 1px 3px rgba(0,0,0,.2),0 4px 12px rgba(0,0,0,.08);isolation:isolate;z-index:0;overflow:hidden}
.tf-docx-body{position:relative;display:flex;flex-direction:column}
.tf-docx-flow{flex:none}
.tf-docx-flow::after,.tf-docx-seg::after,.tf-docx-note::after,.tf-docx-hdr::after,.tf-docx-ftr::after{content:"";display:block;clear:both}
.tf-docx-vcenter{margin:auto 0}
.tf-docx-vbottom{margin-top:auto}
.tf-docx-seg{column-fill:balance}
.tf-docx-hdr,.tf-docx-ftr{position:absolute;z-index:-1}
.tf-docx-host .tf-docx-hdr,.tf-docx-host .tf-docx-ftr{position:relative;z-index:auto}
.tf-docx-host{position:absolute;left:0;top:0;width:0;height:0;overflow:hidden;visibility:hidden;pointer-events:none}
.tf-docx-galley{position:absolute;left:0;top:0}
.tf-docx-p{white-space:pre-wrap;overflow-wrap:break-word;orphans:1;widows:1}
.tf-docx-rel{position:relative}
.tf-docx-marker{white-space:pre}
.tf-docx-tab{display:inline-block;height:1em;vertical-align:baseline;white-space:pre;text-indent:0;background-repeat:no-repeat}
.tf-docx-lead-dot{background-image:radial-gradient(circle,currentColor .05em,transparent .07em);background-size:.3em .2em;background-repeat:repeat-x;background-position:right bottom}
.tf-docx-lead-mid{background-image:radial-gradient(circle,currentColor .05em,transparent .07em);background-size:.3em .2em;background-repeat:repeat-x;background-position:right 70%}
.tf-docx-lead-dash{background-image:repeating-linear-gradient(90deg,currentColor 0 .3em,transparent .3em .45em);background-size:100% .06em;background-position:0 100%}
.tf-docx-lead-line{background-image:linear-gradient(currentColor,currentColor);background-size:100% .06em;background-position:0 100%}
.tf-docx-sup{vertical-align:super}
.tf-docx-ins{color:#1b7a3a!important;text-decoration-line:underline!important}
.tf-docx-del{color:#c62828!important;text-decoration-line:line-through!important}
.tf-docx-cmt{background-color:rgba(255,213,79,.4)}
.tf-docx a.tf-docx-link{color:inherit;text-decoration:none;cursor:pointer}
.tf-docx-inl{display:inline-block;line-height:0;vertical-align:baseline;text-indent:0}
.tf-docx-pic{display:inline-block;position:relative;overflow:hidden;vertical-align:bottom}
.tf-docx-pic img{display:block;max-width:none}
.tf-docx-shape,.tf-docx-group,.tf-docx-chart{display:inline-block;position:relative;vertical-align:bottom;text-indent:0}
.tf-docx-svg{position:absolute;left:0;top:0;overflow:visible}
.tf-docx-txbx{position:absolute;left:0;top:0;right:0;bottom:0;display:flex;flex-direction:column;overflow:hidden;line-height:1.2;white-space:normal;text-align:left;text-indent:0}
.tf-docx-txbx-in{flex:none;min-width:0}
.tf-docx-gchild{position:absolute!important;transform-origin:center}
.tf-docx-obj{display:block;text-indent:0;white-space:normal}
.tf-docx-abs{position:absolute;line-height:0}
.tf-docx-behind{z-index:-1}
.tf-docx-float,.tf-docx-block{line-height:0}
.tf-docx-block{display:block;width:max-content;max-width:100%}
.tf-docx-ph{display:inline-block;background:#eceff1;border:1px solid #cfd8dc}
.tf-docx-hr{display:block}
.tf-docx-wm{display:flex;align-items:center;justify-content:center;white-space:nowrap;line-height:1;pointer-events:none}
.tf-docx-tbl{border-collapse:collapse;border-spacing:0}
.tf-docx-tbl td{vertical-align:top;overflow-wrap:anywhere}
.tf-docx-exact td{overflow:hidden}
.tf-docx-diag{position:absolute;left:0;top:0;width:100%;height:100%;pointer-events:none;overflow:visible}
.tf-docx-gap{border:none!important;padding:0!important}
.tf-docx-vert{writing-mode:vertical-rl;margin:0 auto}
.tf-docx-btlr{transform:rotate(180deg)}
.tf-docx-fn{margin-top:auto;padding-top:8px;flex:none}
.tf-docx-fnrule{width:33%;border-top:.75pt solid #000;margin-bottom:4px}
.tf-docx-endnotes{margin-top:12pt}
.tf-docx-pgborder{position:absolute;pointer-events:none}
.tf-docx-math{font-family:"Cambria Math","STIX Two Math","Latin Modern Math","Times New Roman",serif;white-space:nowrap;display:inline-block;text-indent:0;line-height:1.2;vertical-align:middle}
.tf-docx-math-para{display:block;text-align:center;margin:.2em 0}
.tf-docx-m-row{display:inline-block;vertical-align:middle}
.tf-docx-m-i{font-style:italic}
.tf-docx-m-frac{display:inline-flex;flex-direction:column;vertical-align:middle;text-align:center;margin:0 .1em}
.tf-docx-m-num{border-bottom:1px solid currentColor;padding:0 .15em}
.tf-docx-m-den{padding:0 .15em}
.tf-docx-m-scripts{display:inline-flex;flex-direction:column;vertical-align:middle;font-size:.7em;line-height:1.1}
.tf-docx-math sup,.tf-docx-math sub{font-size:.7em}
.tf-docx-m-scripts sup,.tf-docx-m-scripts sub{font-size:1em;vertical-align:baseline}
.tf-docx-m-under{display:inline-flex;flex-direction:column;align-items:center;vertical-align:middle;line-height:1.1}
.tf-docx-m-small{font-size:.7em}
.tf-docx-m-op{font-size:1.5em;line-height:1}
.tf-docx-m-rad{border-top:1px solid currentColor;padding:1px .1em 0}
.tf-docx-m-fence{font-size:1.2em}
.tf-docx-m-mat{display:inline-table;vertical-align:middle}
.tf-docx-m-mr{display:table-row}
.tf-docx-m-mc{display:table-cell;padding:0 .3em;text-align:center}
`;
