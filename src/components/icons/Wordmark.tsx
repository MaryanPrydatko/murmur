import { useId } from "react";

const ISLAND =
  "M0 0H128A12 12 0 0 0 116 12V60A36 36 0 0 1 80 96H48A36 36 0 0 1 12 60V12A12 12 0 0 0 0 0Z";
const CAT =
  "M18.4 71V61.8A19.4 19.4 0 0 1 47.03 44.74L49.04 25.94A3.52 3.52 0 0 1 55.72 24.79L61.25 36.36A22.2 22.2 0 0 1 72.75 36.36L78.28 24.79A3.52 3.52 0 0 1 84.96 25.94L87.44 49.13A22.2 22.2 0 0 1 89.2 57.8V59.4A5.4 5.4 0 0 0 94.6 64.8H103.4A6.2 6.2 0 0 1 103.4 77.2H94.6A17.8 17.8 0 0 1 76.8 59.4V57.8A9.8 9.8 0 0 0 57.2 57.8V71A6.2 6.2 0 0 1 44.8 71V61.8A7 7 0 0 0 30.8 61.8V71A6.2 6.2 0 0 1 18.4 71Z";
const LETTERS =
  "M167.5 71.7V35.02M167.5 49.02A14 14 0 0 1 195.5 49.02V71.7M195.5 49.02A14 14 0 0 1 223.5 49.02V71.7M246.18 35.02V57.7A14 14 0 0 0 274.18 57.7M274.18 35.02V71.7M296.86 71.7V35.02M296.86 49.02A14 14 0 0 1 321.89 40.4M333.93 71.7V35.02M333.93 49.02A14 14 0 0 1 361.93 49.02V71.7M361.93 49.02A14 14 0 0 1 389.93 49.02V71.7M412.61 35.02V57.7A14 14 0 0 0 440.61 57.7M440.61 35.02V71.7M463.29 71.7V35.02M463.29 49.02A14 14 0 0 1 488.32 40.4";

/**
 * The mark's paths, on a 128 x 96 grid: the ink island and the pink cat. The
 * island is ink in both themes; on dark it gets a 1px light edge (`mark-edge`).
 */
export const MarkPaths = () => {
  const clip = useId();
  return (
    <>
      <clipPath id={clip}>
        <path d={ISLAND} />
      </clipPath>
      <path fill="#141312" d={ISLAND} />
      <path
        fill="none"
        className="stroke-mark-edge"
        strokeWidth="2"
        vectorEffect="non-scaling-stroke"
        clipPath={`url(#${clip})`}
        d={ISLAND}
      />
      <path className="fill-logo-primary" d={CAT} />
    </>
  );
};

/** The lockup: the mark and the drawn letters, which take the text color. */
const Wordmark = ({
  width,
  className = "",
}: {
  width: number;
  className?: string;
}) => (
  <svg
    width={width}
    viewBox="0 0 494 96"
    className={className}
    role="img"
    aria-label="Murmur"
    xmlns="http://www.w3.org/2000/svg"
  >
    <MarkPaths />
    <path
      fill="none"
      stroke="currentColor"
      strokeWidth="11"
      strokeLinecap="round"
      strokeLinejoin="round"
      d={LETTERS}
    />
  </svg>
);

export default Wordmark;
