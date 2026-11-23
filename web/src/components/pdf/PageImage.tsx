/** A picture of one page of a PDF, drawn once it scrolls into view and drawn again when it is turned */
import { useEffect, useRef, useState } from "react";
import type { PDFDocumentProxy, RenderTask } from "pdfjs-dist/legacy/build/pdf.mjs";
import { inTurn } from "@/components/pdf/Thumbnails";

/** `page` counts from 1; `turn` is degrees clockwise, on top of the page's own rotation; `width` is in CSS pixels */
export function PageImage({ doc, page, turn, width }: { doc: PDFDocumentProxy; page: number; turn: number; width: number }) {
  const box = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [visible, setVisible] = useState(false);
  const [ratio, setRatio] = useState(1.294);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const seen = new IntersectionObserver(([e]) => e.isIntersecting && setVisible(true), { rootMargin: "300px 0px" });
    seen.observe(el);
    return () => seen.disconnect();
  }, []);

  useEffect(() => {
    if (!visible) return;
    let stopped = false;
    let task: RenderTask | undefined;
    void inTurn(async () => {
      if (stopped || !canvas.current) return;
      const p = await doc.getPage(page);
      const rotation = (((p.rotate + turn) % 360) + 360) % 360;
      const base = p.getViewport({ scale: 1, rotation });
      const viewport = p.getViewport({ scale: (width * (window.devicePixelRatio || 1)) / base.width, rotation });
      if (stopped || !canvas.current) return;
      setRatio(base.height / base.width);
      canvas.current.width = Math.round(viewport.width);
      canvas.current.height = Math.round(viewport.height);
      task = p.render({ canvas: canvas.current, viewport, background: "#ffffff" });
      await task.promise;
    }).catch(() => undefined);
    return () => {
      stopped = true;
      task?.cancel();
    };
  }, [visible, doc, page, turn, width]);

  return (
    <div ref={box} className="flex items-center justify-center rounded-sm bg-white shadow-sm ring-1 ring-border" style={{ width, height: width * ratio }}>
      <canvas ref={canvas} className="size-full" aria-hidden="true" />
    </div>
  );
}
