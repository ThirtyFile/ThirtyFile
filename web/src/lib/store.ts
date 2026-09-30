//! A value kept outside React (an open question, the tabs, the uploads, a preference): code anywhere reads and changes
//! it, and components that show it with useStore() render again when it changes

import { useSyncExternalStore } from "react";

export interface Store<T> {
  get(): T;
  /** Replaces the value and tells every listener */
  set(next: T): void;
  /** Tells every listener without a new value: the value changed in place, or what it is read from changed */
  emit(): void;
  subscribe(listener: () => void): () => void;
}

export function createStore<T>(initial: T): Store<T> {
  let value = initial;
  const listeners = new Set<() => void>();
  const emit = () => listeners.forEach((l) => l());
  return {
    get: () => value,
    set(next) {
      value = next;
      emit();
    },
    emit,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}

/** The store's value, in a component that renders again when it changes */
export function useStore<T>(store: Store<T>): T {
  return useSyncExternalStore(store.subscribe, store.get);
}
