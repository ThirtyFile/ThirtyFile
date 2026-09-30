import { useQueryClient } from "@tanstack/react-query";
import { StorageLocations } from "@/admin/storage/StorageLocations";
import { t } from "@/lib/i18n";
import { Section, SettingsFrame } from "@/admin/SettingsFrame";
import { keys } from "@/api/queryKeys";

export function StorageSettingsPage() {
  const qc = useQueryClient();
  return (
    <SettingsFrame item="storage" onRefresh={() => qc.invalidateQueries({ queryKey: keys.storageLocations() })}>
      <Section title={t("Storage locations")}>
        <StorageLocations />
      </Section>
    </SettingsFrame>
  );
}
