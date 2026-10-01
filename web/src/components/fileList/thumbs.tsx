//! Thumbnails: the server's, or made in the browser for PDFs and videos

import { useEffect, useRef, useState } from "react";
import type { FileSource, Node } from "@/api";
import { FileIcon, canBrowserThumbnail, canThumbnail } from "@/components/FileIcon";
import { browserThumb, knownThumb } from "@/lib/thumbs";
import { cn } from "@/lib/utils";

export function Thumb({ node, source, className, iconClass }: { node: Node; source: FileSource; className?: string; iconClass?: string }) {
  const [failed, setFailed] = useState(false);
  if (canBrowserThumbnail(node)) return <BrowserThumb node={node} source={source} className={className} iconClass={iconClass} />;
  if (canThumbnail(node) && !failed) {
    return <img src={source.thumbUrl(node)} loading="lazy" draggable={false} onError={() => setFailed(true)} className={cn("object-contain", className)} alt="" />;
  }
  return <FileIcon node={node} className={iconClass} />;
}

/** A PDF's or video's thumbnail: the server's, or made here once the item is on screen (lib/thumbs.ts); the icon until then */
export function BrowserThumb({ node, source, className, iconClass }: { node: Node; source: FileSource; className?: string; iconClass?: string }) {
  const [url, setUrl] = useState<string | null | undefined>(() => knownThumb(node, source));
  const box = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    const known = knownThumb(node, source);
    setUrl(known);
    if (known !== undefined || !box.current) return;
    let job: ReturnType<typeof browserThumb> | null = null;
    let cancelled = false;
    const seen = new IntersectionObserver(
      (entries) => {
        if (job || !entries.some((e) => e.isIntersecting)) return;
        job = browserThumb(node, source);
        void job.promise.then((u) => !cancelled && setUrl(u));
      },
      { rootMargin: "200px" },
    );
    seen.observe(box.current);
    return () => {
      cancelled = true;
      seen.disconnect();
      job?.release();
    };
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- the node object is new after every refresh of the list: its id and date say when the file changed
  }, [node.id, node.updated_at, source]);
  if (url) return <img src={url} draggable={false} onError={() => setUrl(null)} className={cn("object-contain", className)} alt="" />;
  return (
    <span ref={box} className="inline-flex shrink-0">
      <FileIcon node={node} className={iconClass} />
    </span>
  );
}
