import { Fragment } from "react";

/** Where a search found a file: the text around the match, with the match marked (`pieces`: the odd ones are the match) */
export function Snippet({ pieces }: { pieces: string[] }) {
  return (
    <span className="block truncate" title={pieces.join("")}>
      {pieces.map((piece, i) => (
        <Fragment key={i}>{i % 2 ? <mark className="rounded-sm bg-brand/25 px-0.5 text-foreground">{piece}</mark> : piece}</Fragment>
      ))}
    </span>
  );
}
