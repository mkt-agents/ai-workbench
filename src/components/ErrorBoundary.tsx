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
            color: "var(--text-primary, #eee)",
            background: "var(--bg-primary, #1e1e1e)",
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
              background: "rgba(255,80,80,0.1)",
              border: "1px solid rgba(255,80,80,0.4)",
              borderRadius: 6,
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
                borderRadius: 6,
                border: "1px solid var(--border, #444)",
                background: "var(--accent, #4a90e2)",
                color: "#fff",
                cursor: "pointer",
              }}
            >
              重试
            </button>
            <button
              onClick={() => window.location.reload()}
              style={{
                padding: "6px 14px",
                borderRadius: 6,
                border: "1px solid var(--border, #444)",
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
