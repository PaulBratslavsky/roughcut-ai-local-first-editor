// The last transcription error of each project (TanStack Store, like
// viewStore). An import transcribes after the app has already switched to
// the new project, so the code that started it is often gone by the time it
// fails; the error is kept here for the project's transcript panel (next to
// its Transcribe button) and the Shorts screen to show. Per project,
// ephemeral (not persisted).

import { Store } from "@tanstack/react-store";

export const transcribeErrors = new Store<Record<string, string>>({});

/** Set (or clear, with null) a project's transcription error. */
export function setTranscribeError(projectId: string, message: string | null): void {
  transcribeErrors.setState((all) => {
    const next = { ...all };
    if (message === null) delete next[projectId];
    else next[projectId] = message;
    return next;
  });
}
