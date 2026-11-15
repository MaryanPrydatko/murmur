/** The text wordmark until the drawn logo lands. A brand name, not copy. */
const Wordmark = ({ className = "" }: { className?: string }) => (
  // eslint-disable-next-line i18next/no-literal-string
  <span className={`wordmark ${className}`}>murmur</span>
);

export default Wordmark;
