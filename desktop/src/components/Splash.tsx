import { BrandMark } from "./BrandMark";
import wordmarkUrl from "../assets/grokptah-wordmark.svg";

type Props = {
  subtitle?: string;
};

/** First-paint / idle splash — GrokPtah only. */
export function Splash({
  subtitle = "Desktop coding agent",
}: Props) {
  return (
    <div className="splash" role="status" aria-label="GrokPtah">
      <BrandMark size={72} className="splash-mark" />
      <img
        src={wordmarkUrl}
        alt="GrokPtah"
        className="splash-wordmark"
        draggable={false}
      />
      <p className="splash-sub">{subtitle}</p>
    </div>
  );
}
