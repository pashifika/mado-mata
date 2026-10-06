import {useLayoutEffect, useRef} from 'react';
import type {ReactNode} from 'react';

// Only navigation is pinned. Its owning shell shares this clearance with page reveals and the Edit rail.
export default function NavigationHeader({children}: {children: ReactNode}) {
  const header = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const element = header.current;
    const shell = element?.parentElement;
    if (!element || !shell) return;
    const page = element.ownerDocument.documentElement;
    const previousPadding = page.style.scrollPaddingTop;
    const measure = () => {
      const height = element.getBoundingClientRect().height;
      shell.style.setProperty('--navigation-clearance', `${height}px`);
      // The document's focus/scrollIntoView boundary changes; nested tree and editor scrollers do not.
      page.style.scrollPaddingTop = `${height}px`;
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => {
      observer.disconnect();
      shell.style.removeProperty('--navigation-clearance');
      page.style.scrollPaddingTop = previousPadding;
    };
  }, []);
  return <div ref={header} className="navigation-header">{children}</div>;
}
