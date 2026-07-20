interface BannerProps {
  kind: "error" | "info";
  text: string;
  onDismiss: () => void;
}

export default function Banner({ kind, text, onDismiss }: BannerProps) {
  return (
    <div className={`banner ${kind}`}>
      <span>{text}</span>
      <button className="dismiss" onClick={onDismiss}>
        ✕
      </button>
    </div>
  );
}
