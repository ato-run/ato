import { cn } from "./utils";

const GRADIENT_CLASSES = {
  accent: "bg-gradient-to-r from-[#FF905A] to-[#F43F5E] text-white shadow-lg shadow-rose-500/25",
  blue: "bg-gradient-to-r from-[#4A86FF] to-[#2B60FF] text-white shadow-lg shadow-blue-500/25",
};

export default function GradientButton({
  children,
  variant = "accent",
  size = "lg",
  className,
  disabled,
  onClick,
  ...props
}) {
  const sizeClasses = {
    sm: "px-2.5 py-1.5 rounded-lg text-[10px] font-bold",
    md: "px-4 py-2 rounded-xl text-[13px] font-bold",
    lg: "w-full py-4 rounded-2xl font-bold text-[17px]",
  };

  return (
    <button
      className={cn(
        GRADIENT_CLASSES[variant] || GRADIENT_CLASSES.accent,
        sizeClasses[size] || sizeClasses.lg,
        "transition-opacity hover:opacity-90 disabled:opacity-50 disabled:cursor-not-allowed",
        className,
      )}
      disabled={disabled}
      onClick={onClick}
      {...props}
    >
      {children}
    </button>
  );
}
