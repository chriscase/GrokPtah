import { useEffect, useRef, useState } from "react";
import {
  MATERIALIZE_TICK_MS,
  materializeBatchSize,
  tokenizeForMaterialize,
} from "./materialize";

/**
 * `visible` lags behind `target` so large SSE dumps still “arrive” word by word.
 * When `active` is false, snaps to full `target`.
 */
export function useMaterializingText(
  target: string,
  active: boolean,
): { visible: string; pending: number } {
  const [visible, setVisible] = useState(active ? "" : target);
  const visibleRef = useRef(active ? "" : target);
  const queueRef = useRef<string[]>([]);
  const targetRef = useRef(target);
  const [, setTick] = useState(0); // force re-render for pending count

  useEffect(() => {
    targetRef.current = target;

    if (!active) {
      queueRef.current = [];
      visibleRef.current = target;
      setVisible(target);
      return;
    }

    // Rewind / replace
    if (!target.startsWith(visibleRef.current)) {
      // Allow partial prefix match when concurrent updates race
      const common = commonPrefixLen(visibleRef.current, target);
      visibleRef.current = target.slice(0, common);
      queueRef.current = tokenizeForMaterialize(target.slice(common));
      setVisible(visibleRef.current);
      return;
    }

    // Enqueue only what's not already visible or queued
    const accounted =
      visibleRef.current.length +
      queueRef.current.reduce((n, t) => n + t.length, 0);
    if (target.length > accounted) {
      const delta = target.slice(accounted);
      queueRef.current.push(...tokenizeForMaterialize(delta));
    }
  }, [target, active]);

  // Drain queue on a timer while active
  useEffect(() => {
    if (!active) return;

    const id = window.setInterval(() => {
      const q = queueRef.current;
      if (q.length === 0) {
        // Ensure we're fully caught up to target
        if (visibleRef.current !== targetRef.current) {
          // Catch any desync
          if (targetRef.current.startsWith(visibleRef.current)) {
            const rest = targetRef.current.slice(visibleRef.current.length);
            if (rest) q.push(...tokenizeForMaterialize(rest));
            else return;
          } else {
            visibleRef.current = targetRef.current;
            setVisible(targetRef.current);
            return;
          }
        } else {
          return;
        }
      }

      const n = materializeBatchSize(q.length);
      const take = q.splice(0, n).join("");
      visibleRef.current += take;
      setVisible(visibleRef.current);
      setTick((t) => t + 1);
    }, MATERIALIZE_TICK_MS);

    return () => window.clearInterval(id);
  }, [active]);

  // Flush immediately when stream ends
  useEffect(() => {
    if (active) return;
    queueRef.current = [];
    visibleRef.current = target;
    setVisible(target);
  }, [active, target]);

  const pending = queueRef.current.reduce((n, t) => n + t.length, 0);
  return { visible, pending };
}

function commonPrefixLen(a: string, b: string): number {
  const n = Math.min(a.length, b.length);
  let i = 0;
  while (i < n && a[i] === b[i]) i++;
  return i;
}
