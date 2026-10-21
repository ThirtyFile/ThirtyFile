import type { FileSource, Node } from "@/api";
import OfficeWorkspace from "./OfficeWorkspace";

export default function OfficeEditor(props: { node: Node; source: FileSource; onExit(): void; onSaved(node: Node): void }) {
  return <OfficeWorkspace {...props} editable />;
}
