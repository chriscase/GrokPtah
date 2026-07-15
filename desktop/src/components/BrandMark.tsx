import markUrl from "../assets/grokptah-mark.svg";

type Props = {
  size?: number;
  className?: string;
  title?: string;
};

/** GrokPtah product mark — never reuse NexaDeck or other product art. */
export function BrandMark({ size = 22, className, title = "GrokPtah" }: Props) {
  return (
    <img
      src={markUrl}
      width={size}
      height={size}
      alt={title}
      className={className}
      draggable={false}
    />
  );
}
