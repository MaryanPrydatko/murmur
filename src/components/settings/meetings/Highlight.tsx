import React from "react";
import { matchRanges } from "./meetingModel";

/** `text` with each occurrence of `query` marked; `current` gets an outline. */
export const Highlight: React.FC<{
  text: string;
  query: string;
  /** Index of the match among this text's matches to mark as current. */
  current?: number;
}> = ({ text, query, current = -1 }) => {
  const ranges = matchRanges(text, query);
  if (!ranges.length) return <>{text}</>;
  const parts: React.ReactNode[] = [];
  let at = 0;
  ranges.forEach(([a, b], i) => {
    if (a > at) parts.push(text.slice(at, a));
    parts.push(
      <mark key={a} className={i === current ? "cur" : undefined}>
        {text.slice(a, b)}
      </mark>,
    );
    at = b;
  });
  if (at < text.length) parts.push(text.slice(at));
  return <>{parts}</>;
};
