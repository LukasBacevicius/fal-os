import { Component, type ReactNode } from 'react';

/** Error boundary for overlays: a crash in one overlay logs and hides it instead of
 *  unmounting the canvas (and its GPU surface) with it. It retries on the next change. */
export class Guard extends Component<{ name: string; children: ReactNode }, { failed: boolean }> {
  state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: unknown) {
    console.error(`[${this.props.name}] crashed`, error);
    // Give it another chance after the current interaction settles.
    setTimeout(() => this.setState({ failed: false }), 1000);
  }

  render() {
    return this.state.failed ? null : this.props.children;
  }
}
