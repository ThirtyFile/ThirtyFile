import type { FileSource, Node } from "@/api";
import { t } from "@/lib/i18n";
import { MAX_OFFICE_PREVIEW_BYTES, MAX_OFFICE_PREVIEW_LABEL } from "@/lib/officeLimits";
import OfficeWorkspace from "@/components/office/OfficeWorkspace";

/** All Office previews use the packaged DOM/Lit editor in readonly mode, including shares and versions. */
export default function OfficeViewer({ node, source, allowDownload = true }: { node: Node; source: FileSource; allowDownload?: boolean }) {
  if (node.size > MAX_OFFICE_PREVIEW_BYTES)
    return (
      <p className="p-6 text-center text-sm text-destructive">{t("The file is too large (over {size}) to preview online. Download it to open it.", { size: MAX_OFFICE_PREVIEW_LABEL })}</p>
    );
  return <OfficeWorkspace node={node} source={source} allowDownload={allowDownload} />;
}
