import React from "react";
import { MarkPaths } from "../icons/Wordmark";

/** The mark (island and cat), `size` px wide. */
export const Mascot: React.FC<{ size: number }> = ({ size }) => (
  <svg
    width={size}
    viewBox="0 0 128 96"
    className="block select-none"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <MarkPaths />
  </svg>
);
