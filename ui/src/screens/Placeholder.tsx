import { useTranslation } from "react-i18next";

export function Placeholder() {
  const { t } = useTranslation();
  return (
    <div className="placeholder">
      <p className="muted">{t("placeholder")}</p>
    </div>
  );
}
