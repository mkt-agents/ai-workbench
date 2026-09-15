import logoIce from "../assets/logo-ice.png";
import logoSilver from "../assets/logo-silver.png";

type Props = {
  size?: number;
  /** Default cool silver; ice is the alternate brand mark. */
  variant?: "ice" | "silver";
  className?: string;
  title?: string;
};

/** Brand mark from design masters (docs/brand → src/assets). */
export default function AppLogoMark({
  size = 28,
  variant = "silver",
  className,
  title = "AI Workbench",
}: Props) {
  const src = variant === "ice" ? logoIce : logoSilver;

  return (
    <img
      src={src}
      width={size}
      height={size}
      alt={title}
      title={title}
      className={className}
      draggable={false}
    />
  );
}
