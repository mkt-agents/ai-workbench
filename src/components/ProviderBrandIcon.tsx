import { useEffect, useState } from "react";
import { aiProviderLogoSrc } from "../lib/aiProviderLogos";
import { getProviderMeta, providerIconLabel } from "../lib/aiProviders";

type Props = {
  provider: string;
  className?: string;
  title?: string;
};

/** Provider brand tile with onError fallback to shortLabel. */
export default function ProviderBrandIcon({ provider, className, title }: Props) {
  const logoSrc = aiProviderLogoSrc(provider);
  const [broken, setBroken] = useState(false);
  useEffect(() => {
    setBroken(false);
  }, [logoSrc]);

  const meta = getProviderMeta(provider);
  const iconClass = meta ? ("provider-" + provider) : "provider-custom";
  const showLogo = Boolean(logoSrc) && !broken;
  const label = title ?? meta?.label ?? provider;
  const tileClass =
    (className ?? "ai-model-icon") + " " + iconClass + (showLogo ? " has-logo" : "");

  return (
    <div className={tileClass} title={label}>
      {showLogo ? (
        <img src={logoSrc!} alt="" draggable={false} onError={() => setBroken(true)} />
      ) : (
        providerIconLabel(provider)
      )}
    </div>
  );
}