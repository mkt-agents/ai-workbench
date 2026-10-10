import { Component, type ReactNode } from "react";

interface Props {
  children: ReactNode;
  fallback?: ReactNode;
}

interface State {
  error: Error | null;
  errorInfo: { componentStack: string } | null;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, errorInfo: null };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, errorInfo: { componentStack: string }) {
    this.setState({ errorInfo });
    console.error("[ErrorBoundary]", error, errorInfo.componentStack);
  }

  handleReset = () => {
    this.setState({ error: null, errorInfo: null });
  };

  render() {
    const { error, errorInfo } = this.state;
    if (error) {
      if (this.props.fallback) return this.props.fallback;
      return (
        <div
          style={{
            padding: 24,
            fontFamily: "system-ui, sans-serif",
            color: "var(--text-0)",
            background: "var(--bg-1)",
            minHeight: "100vh",
            display: "flex",
            flexDirection: "column",
            gap: 12,
          }}
        >
          <h2 style={{ margin: 0, fontSize: 18 }}>页面出现错误</h2>
          <pre
            style={{
              padding: 12,
              background: "var(--red-dim)",
              border: "1px solid var(--red-border)",
              borderRadius: "var(--radius-sm)",
              whiteSpace: "pre-wrap",
              wordBreak: "break-word",
              fontSize: 13,
              maxHeight: 240,
              overflow: "auto",
            }}
          >
            {error.message}
          </pre>
          {errorInfo && (
            <details>
              <summary style={{ cursor: "pointer", fontSize: 12, opacity: 0.7 }}>
                组件堆栈
              </summary>
              <pre style={{ fontSize: 11, opacity: 0.6, whiteSpace: "pre-wrap" }}>
                {errorInfo.componentStack}
              </pre>
            </details>
          )}
          <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
            <button
              onClick={this.handleReset}
              style={{
                padding: "6px 14px",
                borderRadius: "var(--radius-sm)",
                border: "1px solid var(--border)",
                background: "var(--accent)",
                color: "var(--on-accent)",
                cursor: "pointer",
              }}
            >
              重试
            </button>
            <button
              onClick={() => window.location.reload()}
              style={{
                padding: "6px 14px",
                borderRadius: "var(--radius-sm)",
                border: "1px solid var(--border)",
                background: "transparent",
                color: "inherit",
                cursor: "pointer",
              }}
            >
              刷新页面
            </button>
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}

export default ErrorBoundary;
