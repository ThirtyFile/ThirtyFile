import { Link } from "react-router";
import { FileQuestionIcon, LayersIcon } from "lucide-react";
import { Frame } from "@/components/Frame";
import { buttonVariants } from "@/components/ui/button";
import { t } from "@/lib/i18n";

/** An address that is no page here (a link mistyped, or from an older version): says so, rather than open another page */
export function NotFoundPage() {
  return (
    <Frame toolbar={null} crumbs={[{ label: t("Page not found") }]} icon={FileQuestionIcon} upTo={null}>
      <div role="alert" className="flex min-h-52 flex-1 flex-col items-center justify-center gap-3 p-6 text-center">
        <FileQuestionIcon className="size-9 stroke-[1.4] text-muted-foreground" />
        <div className="grid max-w-md gap-1">
          <p className="font-medium">{t("Page not found")}</p>
          <p className="text-sm text-muted-foreground">{t("There's no page at this address. Check the link, or start from All spaces.")}</p>
        </div>
        <Link to="/drives" className={buttonVariants({ variant: "outline", size: "sm" })}>
          <LayersIcon /> {t("Go to All spaces")}
        </Link>
      </div>
    </Frame>
  );
}
