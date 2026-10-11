/**
 * Writing WordprocessingML elements in a document being changed (tables, formatting): made in the document's own
 * namespace and prefix (transitional or strict), and put where Word's schema says among their siblings, as Word refuses
 * elements in the wrong order.
 */

export class WordXml {
  readonly ns: string;
  readonly prefix: string | null;
  constructor(
    readonly doc: Document,
    sample: Element,
  ) {
    this.ns = sample.namespaceURI ?? "";
    this.prefix = sample.prefix;
  }

  el(name: string, val?: string | number) {
    const e = this.doc.createElementNS(this.ns, this.prefix ? `${this.prefix}:${name}` : name);
    if (val !== undefined) this.set(e, "val", String(val));
    return e;
  }

  /** Sets a w: attribute (an existing one by its local name, for a parser that keeps prefixes in names) */
  set(e: Element, name: string, value: string) {
    const existing = Array.from(e.attributes).find((a) => a.localName === name || a.name.endsWith(`:${name}`));
    if (existing) existing.value = value;
    else e.setAttributeNS(this.ns, this.prefix ? `${this.prefix}:${name}` : name, value);
  }

  /** Removes w: attributes by their local names */
  unset(e: Element, ...names: string[]) {
    for (const a of Array.from(e.attributes)) if (names.some((n) => a.localName === n || a.name.endsWith(`:${n}`))) e.removeAttributeNode(a);
  }

  /** An element's attributes all removed, to set them afresh */
  clear(e: Element) {
    for (const a of Array.from(e.attributes)) e.removeAttributeNode(a);
    return e;
  }
}

/** Puts `child` into `parent` where `order` says: before the first child that comes after it */
export function placeIn(parent: Element, child: Element, order: string[]): Element {
  const i = order.indexOf(child.localName);
  const later = Array.from(parent.children).find((c) => order.indexOf(c.localName) > i);
  if (later) parent.insertBefore(child, later);
  else parent.append(child);
  return child;
}

/** Where a part's blocks are: document.xml's body, or a header's or footer's root */
export function storyRoot(doc: Document | null): Element | null {
  const root = doc?.documentElement;
  if (!root) return null;
  if (root.localName === "hdr" || root.localName === "ftr") return root;
  return Array.from(root.children).find((c) => c.localName === "body") ?? null;
}
