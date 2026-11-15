/** The tray icon, the paths of the tray_*_dark.svg brand files; uses currentColor like a macOS template image. */
type TrayIconState = "idle" | "recording" | "transcribing";

export const PATHS: Record<TrayIconState, string> = {
  idle: "M0 13V11.5A3.5 3.5 0 0 1 5.29 8.49A4 4 0 0 1 5.35 8.37L5.87 4.23A0.4 0.4 0 0 1 6.59 4.05L8.03 6.12A4 4 0 0 1 9.97 6.12L11.41 4.05A0.4 0.4 0 0 1 12.13 4.23L12.65 8.37A4 4 0 0 1 13 10V11.56A0.44 0.44 0 0 0 13.44 12H17A1 1 0 0 1 17 14H13.44A2.44 2.44 0 0 1 11 11.56V10A2 2 0 0 0 7 10V13A1 1 0 0 1 5 13V11.5A1.5 1.5 0 0 0 2 11.5V13A1 1 0 0 1 0 13Z",
  recording:
    "M0 13V11.5A3.5 3.5 0 0 1 5.29 8.49L6.18 3.69A0.4 0.4 0 0 1 6.93 3.58L8.17 6.09A4 4 0 0 1 9.83 6.09L11.07 3.58A0.4 0.4 0 0 1 11.82 3.69L12.71 8.5A4 4 0 0 1 13 10V11.56A0.44 0.44 0 0 0 13.44 12H17A1 1 0 0 1 17 14H6A1 1 0 0 1 5 13V11.5A1.5 1.5 0 0 0 2 11.5V13A1 1 0 0 1 0 13ZM6 9.5V10.5A0.5 0.5 0 0 0 6.5 11H7.5A0.5 0.5 0 0 0 8 10.5V9.5A0.5 0.5 0 0 0 7.5 9H6.5A0.5 0.5 0 0 0 6 9.5ZM10 9.5V10.5A0.5 0.5 0 0 0 10.5 11H11.5A0.5 0.5 0 0 0 12 10.5V9.5A0.5 0.5 0 0 0 11.5 9H10.5A0.5 0.5 0 0 0 10 9.5Z",
  transcribing:
    "M0 13V11.5A3.5 3.5 0 0 1 5.29 8.49A4 4 0 0 1 5.35 8.37L5.87 4.23A0.4 0.4 0 0 1 6.59 4.05L8.03 6.12A4 4 0 0 1 9.97 6.12L11.41 4.05A0.4 0.4 0 0 1 12.13 4.23L12.65 8.37A4 4 0 0 1 13 10V11.56A0.44 0.44 0 0 0 13.44 12H17A1 1 0 0 1 17 14H13.44A2.44 2.44 0 0 1 11 11.56V10A2 2 0 0 0 7 10V13A1 1 0 0 1 5 13V11.5A1.5 1.5 0 0 0 2 11.5V13A1 1 0 0 1 0 13ZM2 17A1 1 0 1 0 4 17A1 1 0 1 0 2 17ZM8 17A1 1 0 1 0 10 17A1 1 0 1 0 8 17ZM14 17A1 1 0 1 0 16 17A1 1 0 1 0 14 17Z",
};

const TrayIcon = ({
  state,
  size,
}: {
  state: TrayIconState;
  size: number | string;
}) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 18 18"
    className="fill-current"
    xmlns="http://www.w3.org/2000/svg"
  >
    <path fillRule="evenodd" d={PATHS[state]} />
  </svg>
);

export default TrayIcon;
