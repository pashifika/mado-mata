import {useEffect, useRef, useState} from 'react';
import type {KeyboardEvent, PointerEvent} from 'react';
import {clientToFrame, dragEdges, edgesRect, hitHandle, mapRegion, rectEdges, regionFromEdges, spanEdges} from '../recognition.ts';
import type {Edges, GeometryBasis, Handle, Point, PreviewDefinition, PreviewEdit, PreviewObservation, PreviewTool} from '../recognition.ts';

// CSS pixels around an edge that grab it; converted to frame pixels by the rendered scale.
const HIT_PX = 6;
// A press that moves less than this many CSS pixels is a click, not a new rectangle.
const CREATE_PX = 4;
// Drawn handle size in CSS pixels.
const HANDLE_PX = 8;
const CURSORS: Record<Handle, string> = {
  move: 'move', n: 'ns-resize', s: 'ns-resize', e: 'ew-resize', w: 'ew-resize', ne: 'nesw-resize', sw: 'nesw-resize', nw: 'nwse-resize', se: 'nwse-resize',
};

type Part = 'region' | 'search';
type Drag =
  | {kind: 'region'; id: string; part: Part; revision: number; handle: Handle; start: Edges; origin: Point; current: Edges; bounds: Edges}
  // `handle` null spans a new content rectangle.
  | {kind: 'content'; handle: Handle | null; start: Edges; origin: Point; client: Point; current: Edges | null; bounds: Edges}
  | {kind: 'create'; origin: Point; client: Point; current: Edges | null; bounds: Edges};
// An edit relayed to the main window and shown until the next snapshot replaces it.
interface Pending {key: string; edges: Edges}

export interface CanvasLabels {surface: string; search: (name: string) => string; observed: string}

interface Props {
  width: number; height: number; basis: GeometryBasis;
  definitions: PreviewDefinition[]; selected: string | null; observations: PreviewObservation[];
  // Preview raster (possibly smaller than the frame); it is stretched to the frame's display size.
  image: string | null;
  // CSS pixels per frame pixel.
  scale: number; tool: PreviewTool; editable: boolean;
  // Identity of the latest snapshot; a new one supersedes locally shown pending edits.
  generation: unknown;
  labels: CanvasLabels;
  onEdit: (edit: PreviewEdit) => void;
}

function sameEdges(a: Edges, b: Edges): boolean {
  return a.left === b.left && a.top === b.top && a.right === b.right && a.bottom === b.bottom;
}

function handlePoints(edges: Edges): [Handle, number, number][] {
  const {left, top, right, bottom} = edges;
  const x = (left + right) / 2, y = (top + bottom) / 2;
  return [['nw', left, top], ['n', x, top], ['ne', right, top], ['e', right, y], ['se', right, bottom], ['s', x, bottom], ['sw', left, bottom], ['w', left, y]];
}

// Original-pixel image surface with the content rectangle and recognition regions. All geometry is in frame pixels;
// the SVG viewBox is the frame, so display scale changes only the rendered size. Pointer positions map through the
// rendered client rectangle (zoom, scroll and letterboxing included); device-pixel ratio is not applied again.
export default function RecognitionCanvas({width, height, basis, definitions, selected, observations, image, scale, tool, editable, generation, labels, onEdit}: Props) {
  const surface = useRef<HTMLDivElement>(null);
  const dragRef = useRef<Drag | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [pending, setPending] = useState<Pending | null>(null);
  const [cursor, setCursor] = useState('crosshair');
  const content = rectEdges(basis.content);
  const frame: Edges = {left: 0, top: 0, right: width, bottom: height};

  function update(next: Drag | null) {
    dragRef.current = next;
    setDrag(next);
  }

  // The main window answered (applied or refused): its snapshot is the truth again. A lost answer cannot block
  // editing for longer than a moment.
  useEffect(() => setPending(null), [generation]);
  useEffect(() => {
    if (pending === null) return;
    const timer = setTimeout(() => setPending(null), 2000);
    return () => clearTimeout(timer);
  }, [pending]);
  useEffect(() => {
    if (!editable) update(null);
  }, [editable]);
  useEffect(() => {
    if (drag === null) return;
    const cancel = (event: globalThis.KeyboardEvent) => {if (event.key === 'Escape') update(null);};
    window.addEventListener('keydown', cancel);
    return () => window.removeEventListener('keydown', cancel);
  }, [drag !== null]);

  function locate(event: {clientX: number; clientY: number}): {point: Point; perPixel: number} {
    const rect = surface.current!.getBoundingClientRect();
    return {point: clientToFrame(event.clientX, event.clientY, rect, width, height), perPixel: rect.width / width};
  }

  // Edges currently shown for a region part: an active drag, then a relayed edit, then the snapshot.
  function shown(definition: PreviewDefinition, part: Part): Edges | null {
    const key = `${definition.id}:${part}`;
    if (drag?.kind === 'region' && `${drag.id}:${drag.part}` === key) return drag.current;
    if (pending?.key === key) return pending.edges;
    const region = part === 'region' ? definition.region : definition.search;
    return region ? mapRegion(region, basis) : null;
  }

  function shownContent(): Edges {
    if (drag?.kind === 'content' && drag.current) return drag.current;
    return pending?.key === 'content' ? pending.edges : content;
  }

  // The selected definition's parts first (pattern before its search area), then others from the topmost.
  function target(point: Point, tolerance: number): {definition: PreviewDefinition; part: Part; handle: Handle; edges: Edges} | null {
    const chosen = definitions.find(item => item.id === selected);
    const ordered = chosen ? [chosen, ...definitions.filter(item => item !== chosen).reverse()] : [...definitions].reverse();
    for (const definition of ordered) {
      const parts: Part[] = definition === chosen && definition.search ? ['region', 'search'] : ['region'];
      for (const part of parts) {
        const edges = shown(definition, part);
        const handle = edges && hitHandle(edges, point, tolerance);
        if (edges && handle) return {definition, part, handle, edges};
      }
    }
    return null;
  }

  function hover(point: Point, tolerance: number): Handle | null {
    if (tool === 'content') return hitHandle(shownContent(), point, tolerance);
    return target(point, tolerance)?.handle ?? null;
  }

  function down(event: PointerEvent<HTMLDivElement>) {
    if (!editable || event.button !== 0 || pending !== null) return;
    const {point, perPixel} = locate(event);
    const tolerance = HIT_PX / perPixel;
    const client = {x: event.clientX, y: event.clientY};
    event.currentTarget.setPointerCapture(event.pointerId);
    event.preventDefault();
    if (tool === 'content') {
      const handle = hitHandle(content, point, tolerance);
      update({kind: 'content', handle, start: content, origin: point, client, current: handle ? content : null, bounds: frame});
      return;
    }
    const hit = target(point, tolerance);
    if (hit) {
      if (hit.definition.id !== selected) onEdit({kind: 'select', id: hit.definition.id});
      update({kind: 'region', id: hit.definition.id, part: hit.part, revision: hit.definition.revision, handle: hit.handle, start: hit.edges, origin: point, current: hit.edges, bounds: content});
    } else update({kind: 'create', origin: point, client, current: null, bounds: content});
  }

  function move(event: PointerEvent<HTMLDivElement>) {
    const active = dragRef.current;
    const {point, perPixel} = locate(event);
    if (active === null) {
      if (editable) {
        const handle = hover(point, HIT_PX / perPixel);
        setCursor(handle ? CURSORS[handle] : 'crosshair');
      }
      return;
    }
    const dx = Math.round(point.x - active.origin.x), dy = Math.round(point.y - active.origin.y);
    if (active.kind === 'region') update({...active, current: dragEdges(active.start, active.handle, dx, dy, active.bounds)});
    else if (active.kind === 'content' && active.handle) update({...active, current: dragEdges(active.start, active.handle, dx, dy, active.bounds)});
    else if (Math.hypot(event.clientX - active.client.x, event.clientY - active.client.y) >= CREATE_PX) update({...active, current: spanEdges(active.origin, point, active.bounds)});
  }

  function up() {
    const active = dragRef.current;
    update(null);
    if (active === null) return;
    if (active.kind === 'region') {
      const region = sameEdges(active.current, active.start) ? null : regionFromEdges(active.current, basis);
      if (!region) return;
      setPending({key: `${active.id}:${active.part}`, edges: active.current});
      onEdit({kind: 'region', id: active.id, part: active.part, revision: active.revision, region});
    } else if (active.kind === 'content') {
      if (!active.current || sameEdges(active.current, active.start)) return;
      setPending({key: 'content', edges: active.current});
      onEdit({kind: 'content', content: edgesRect(active.current)});
    } else if (active.current) {
      const region = regionFromEdges(active.current, basis);
      if (region) onEdit({kind: 'create', region});
    } else onEdit({kind: 'select', id: null});
  }

  // Arrow keys move the selected region (or the content rectangle) by one pixel, ten with Shift.
  function key(event: KeyboardEvent<HTMLDivElement>) {
    const step = event.shiftKey ? 10 : 1;
    const delta: Record<string, [number, number]> = {ArrowLeft: [-step, 0], ArrowRight: [step, 0], ArrowUp: [0, -step], ArrowDown: [0, step]};
    const offset = delta[event.key];
    if (!offset || !editable || pending !== null || drag !== null) return;
    event.preventDefault();
    if (tool === 'content') {
      const next = dragEdges(content, 'move', offset[0], offset[1], frame);
      if (sameEdges(next, content)) return;
      setPending({key: 'content', edges: next});
      onEdit({kind: 'content', content: edgesRect(next)});
      return;
    }
    const definition = definitions.find(item => item.id === selected);
    const edges = definition && mapRegion(definition.region, basis);
    if (!definition || !edges) return;
    const next = dragEdges(edges, 'move', offset[0], offset[1], content);
    const region = sameEdges(next, edges) ? null : regionFromEdges(next, basis);
    if (!region) return;
    setPending({key: `${definition.id}:region`, edges: next});
    onEdit({kind: 'region', id: definition.id, part: 'region', revision: definition.revision, region});
  }

  const stroke = {vectorEffect: 'non-scaling-stroke' as const};
  const handleSize = HANDLE_PX / scale;
  function handles(edges: Edges, className: string) {
    return handlePoints(edges).map(([handle, x, y]) => <rect key={handle} className={className} x={x - handleSize / 2} y={y - handleSize / 2}
      width={handleSize} height={handleSize} style={stroke}/>);
  }
  const bars = shownContent();
  const create = drag?.kind === 'create' ? drag.current : drag?.kind === 'content' && !drag.handle ? drag.current : null;
  const chosen = tool === 'zones' ? definitions.find(item => item.id === selected) : undefined;
  const chosenRegion = chosen && shown(chosen, 'region');
  const chosenSearch = chosen?.search ? shown(chosen, 'search') : null;

  return <div ref={surface} className="recognition-surface" tabIndex={0} role="application" aria-label={labels.surface}
    style={{width: width * scale, height: height * scale, cursor: editable ? cursor : 'default'}}
    onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={() => update(null)} onLostPointerCapture={() => update(null)}
    onKeyDown={key}>
    {image && <img src={image} alt="" draggable={false} className={scale >= 2 ? 'pixelated' : undefined}/>}
    <svg className="recognition-overlay" viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" aria-hidden="true">
      <path className="recognition-bars" fillRule="evenodd"
        d={`M0 0H${width}V${height}H0Z M${bars.left} ${bars.top}V${bars.bottom}H${bars.right}V${bars.top}Z`}/>
      <rect className={tool === 'content' ? 'recognition-content active' : 'recognition-content'} x={bars.left} y={bars.top}
        width={bars.right - bars.left} height={bars.bottom - bars.top} style={stroke}/>
      {definitions.map(definition => {
        const edges = shown(definition, 'region');
        const search = definition.search ? shown(definition, 'search') : null;
        const current = definition.id === selected;
        return <g key={definition.id} className={`recognition-zone ${definition.kind}${current ? ' selected' : ''}`}>
          {search && <rect className="recognition-search" x={search.left} y={search.top} width={search.right - search.left} height={search.bottom - search.top} style={stroke}>
            <title>{labels.search(definition.name)}</title></rect>}
          {edges && <rect className="recognition-region" x={edges.left} y={edges.top} width={edges.right - edges.left} height={edges.bottom - edges.top} style={stroke}>
            <title>{definition.name}</title></rect>}
        </g>;
      })}
      {observations.map((item, index) => <rect key={index} className={item.fresh ? 'recognition-observation' : 'recognition-observation stale'}
        x={item.bounds.x} y={item.bounds.y} width={item.bounds.width} height={item.bounds.height} style={stroke}><title>{labels.observed}</title></rect>)}
      {create && <rect className="recognition-draft" x={create.left} y={create.top} width={create.right - create.left} height={create.bottom - create.top} style={stroke}/>}
      {editable && tool === 'content' && handles(bars, 'recognition-handle content')}
      {editable && chosenSearch && handles(chosenSearch, 'recognition-handle search')}
      {editable && chosenRegion && handles(chosenRegion, 'recognition-handle')}
    </svg>
  </div>;
}
