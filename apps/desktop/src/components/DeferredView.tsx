import {Component, Suspense, createContext, useCallback, useContext, useLayoutEffect, useRef} from 'react';
import type {ReactNode} from 'react';

interface Props {
  children: ReactNode;
  fallback: ReactNode;
  error: (cause: unknown) => ReactNode;
}

class ViewErrorBoundary extends Component<Pick<Props, 'children' | 'error'>, {failure: {cause: unknown} | null}> {
  state: {failure: {cause: unknown} | null} = {failure: null};

  static getDerivedStateFromError(cause: unknown) { return {failure: {cause}}; }

  render() {
    return this.state.failure === null ? this.props.children : this.props.error(this.state.failure.cause);
  }
}

const DeferredFocus = createContext<() => boolean>(() => true);

// Initial editor focus may follow the initiating control, but not a focus move made while code was loading.
export function useDeferredFocus(): () => boolean { return useContext(DeferredFocus); }

function Commit({onCommit, children}: {onCommit: () => void; children: ReactNode}) {
  useLayoutEffect(onCommit, [onCommit]);
  return children;
}

// Keep controllers, operation controls and modal owners outside this boundary. Key it by the view's owner,
// not its draft revision; loading and errors must never replace the authoritative session.
export default function DeferredView({children, fallback, error}: Props) {
  const parentFocus = useDeferredFocus();
  const pendingFocus = useRef<{element: Element | null; allowed: boolean} | null>(null);
  const pending = useCallback(() => {
    pendingFocus.current = {element: document.activeElement, allowed: parentFocus()};
  }, [parentFocus]);
  const ready = useCallback(() => { pendingFocus.current = null; }, []);
  const canFocus = useCallback(() => {
    const active = document.activeElement;
    const waiting = pendingFocus.current;
    return parentFocus() && !active?.closest('dialog[open]')
      && (waiting === null || waiting.allowed && (active === waiting.element || active === document.body));
  }, [parentFocus]);

  return <DeferredFocus value={canFocus}>
    <ViewErrorBoundary error={error}>
      <Suspense fallback={<Commit onCommit={pending}>{fallback}</Commit>}>
        <Commit onCommit={ready}>{children}</Commit>
      </Suspense>
    </ViewErrorBoundary>
  </DeferredFocus>;
}
