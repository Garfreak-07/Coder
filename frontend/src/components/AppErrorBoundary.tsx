import { Component, type ErrorInfo, type ReactNode } from "react";

interface AppErrorBoundaryProps {
  children: ReactNode;
  message?: string;
}

interface AppErrorBoundaryState {
  error: Error | null;
  details: string;
}

export class AppErrorBoundary extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = {
    error: null,
    details: ""
  };

  static getDerivedStateFromError(error: Error): Partial<AppErrorBoundaryState> {
    return { error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    this.setState({
      details: `${error.stack ?? error.message}\n${errorInfo.componentStack ?? ""}`.trim()
    });
  }

  retry = () => {
    this.setState({ error: null, details: "" });
  };

  render() {
    if (!this.state.error) return this.props.children;

    return (
      <section className="render-error-panel" role="alert">
        <strong>{this.props.message ?? "Something went wrong while rendering the work timeline."}</strong>
        <p>The rest of the app is still available.</p>
        <div className="render-error-actions">
          <button onClick={this.retry}>Retry</button>
          <details>
            <summary>Show debug details</summary>
            <pre>{this.state.details || this.state.error.message}</pre>
          </details>
        </div>
      </section>
    );
  }
}
