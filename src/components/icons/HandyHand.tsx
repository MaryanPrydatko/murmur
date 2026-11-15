import { PATHS } from "./TrayIcon";

/** The idle cat, in the text color. */
const HandyHand = ({
  width,
  height,
}: {
  width?: number | string;
  height?: number | string;
}) => (
  <svg
    width={width || 18}
    height={height || 18}
    viewBox="0 0 18 18"
    fill="currentColor"
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    <path fillRule="evenodd" d={PATHS.idle} />
  </svg>
);

export default HandyHand;
