import { Component, type ErrorInfo, type ReactNode } from "react";

type Props = {
  children: ReactNode;
  /** Optional label for which surface failed (transcript, pane, app). */
  label?: string;
};

type State = {
  error: Error | null;
};

/**
 * Recoverable React error boundary — prevents one bad bubble/render from
 * blanking the entire Tauri webview (#124).
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error(
      `[GrokPtah] render error (${this.props.label ?? "ui"}):`,
      error,
      info.componentStack,
    );
  }

  render() {
    if (this.state.error) {
      return (
        <div className="error-boundary" role="alert">
          <h3>Something went wrong</h3>
          <p className="error-boundary-msg">
            {this.props.label
              ? `The ${this.props.label} hit a render error.`
              : "A render error was caught."}{" "}
            The rest of the app should still work.
          </p>
          <pre className="error-boundary-detail">
            {this.state.error.message}
          </pre>
          <button
            type="button"
            className="primary"
            onClick={() => this.setState({ error: null })}
          >
            Try again
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
