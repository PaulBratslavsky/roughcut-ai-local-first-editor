// The LOCAL / HOSTED switch where the user works: a chip in the top bar, or
// a one-line "AI: …" on the import screen. Click: with no API key stored (in
// either mode) or a rejected key, open Setup at the Models row — never
// switch; Local → Hosted the first time asks the user to confirm what Hosted
// sends; after that it just toggles.

import { useState } from "react";
import { chooseModelMode, modelChip, useModelStatus } from "../state/modelStore";
import { errText } from "./clipUtil";
import { SetupPanel } from "./SetupPanel";

export function ModelChip({ variant = "chip" }: { variant?: "chip" | "line" }) {
  const status = useModelStatus();
  const [busy, setBusy] = useState(false);
  const [setup, setSetup] = useState<{ notice: string | null } | null>(null);

  if (!status) return null;
  const chip = modelChip(status);

  const onClick = async () => {
    if (busy) return;
    if (chip.setup) {
      setSetup({ notice: null });
      return;
    }
    setBusy(true);
    try {
      const r = await chooseModelMode(status.mode === "local" ? "hosted" : "local");
      if (r === "needs_key") setSetup({ notice: null });
    } catch (e) {
      // Shown where the switch can be fixed.
      setSetup({ notice: errText(e) });
    } finally {
      setBusy(false);
    }
  };

  // "AI: Hosted → Local (offline)" on the import screen.
  const lineLabel = `AI: ${chip.label
    .replace("HOSTED", "Hosted")
    .replace("LOCAL", "Local")}`;

  return (
    <>
      <button
        className={variant === "chip" ? `model-chip ${chip.tone}` : `model-line ${chip.tone}`}
        onClick={() => void onClick()}
        disabled={busy}
        title={chip.title}
        aria-label={`AI model: ${chip.label}. ${chip.title}`}
      >
        {variant === "chip" ? chip.label : lineLabel}
      </button>
      {setup && <SetupPanel focus="models" notice={setup.notice} onClose={() => setSetup(null)} />}
    </>
  );
}
