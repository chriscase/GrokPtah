import markUrl from "../assets/grokptah-mark.svg";

type Props = {
  size?: number;
  className?: string;
  title?: string;
};

/** GrokPtah mark (coding-agent glyph). */
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
