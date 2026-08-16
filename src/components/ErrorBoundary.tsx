// FundLens 顶层错误边界 — 任何渲染期异常都转为可读错误面板，
// 避免白屏（无错误信息）导致无法定位问题。
import { Component, type ErrorInfo, type ReactNode } from 'react';

interface Props {
  children: ReactNode;
}
interface State {
  error: Error | null;
}

export default class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // 同时打到控制台，便于通过 devtools 查看完整堆栈
    console.error('[FundLens] 渲染异常:', error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div className="min-h-screen bg-background text-foreground flex items-center justify-center p-6">
        <div className="max-w-2xl w-full bg-surface border border-danger/40 rounded-md shadow-ring p-5">
          <h1 className="text-lg font-semibold text-danger mb-2">界面渲染出错</h1>
          <p className="text-sm text-muted mb-3">
            应用在渲染时抛出了异常（已捕获，未白屏）。请把下方错误信息截图或复制发我，便于定位修复。
          </p>
          <pre className="text-xs bg-background border border-border rounded p-3 overflow-auto whitespace-pre-wrap text-danger">
            {error.message}
            {'\n\n'}
            {error.stack}
          </pre>
          <button
            onClick={() => this.setState({ error: null })}
            className="mt-3 rounded-md border border-border px-3 py-1.5 text-sm hover:bg-background/60"
          >
            重试
          </button>
        </div>
      </div>
    );
  }
}
