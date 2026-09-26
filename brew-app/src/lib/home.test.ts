import { describe, expect, it } from "vitest";
import type { Freshness, SourceStatus, SyncPhase } from "./api";
import {
  formatRssi,
  formatVolts,
  guessText,
  quietSources,
  silence,
  silenceText,
  sparklinePath,
} from "./home";

function status(
  phase: SyncPhase,
  last_error_code: string | null = null,
): SourceStatus {
  return {
    source_id: 1,
    phase,
    last_success: null,
    last_error: phase === "idle" ? null : "boom",
    last_error_code,
    rows_added: 0,
    newest_reading: null,
    consecutive_failures: phase === "idle" ? 0 : 3,
    next_poll_at: null,
  };
}

describe("silence", () => {
  it("is fresh whatever the sync says when the stream is not stale", () => {
    expect(silence(false, status("unreachable"))).toBe("fresh");
  });
  it("blames the Pi, not the sensor, when brewd is unreachable", () => {
    expect(silence(true, status("unreachable", "fermentrack.timeout"))).toBe(
      "unreachable",
    );
  });
  it("won't call a sensor silent while the sync is failing", () => {
    expect(silence(true, status("error", "fermentrack.http"))).toBe(
      "sync_error",
    );
  });
  it("calls it silent only when the source syncs cleanly", () => {
    expect(silence(true, status("idle"))).toBe("silent");
    expect(silence(true, undefined)).toBe("silent");
  });
  it("goes by the phase, not the error code", () => {
    // A connect failure and a 500 share `fermentrack.http`; only the phase tells them apart.
    expect(silence(true, status("unreachable", "fermentrack.http"))).toBe(
      "unreachable",
    );
  });
});

describe("silenceText", () => {
  it("labels the guess as one", () => {
    expect(silenceText("silent", "battery")).toBe(
      "sensor silent · battery likely (guess)",
    );
    expect(guessText("signal")).toContain("(guess)");
  });
  it("never guesses when it can't see the sensor", () => {
    expect(silenceText("unreachable", "battery")).not.toContain("battery");
    expect(silenceText("fresh")).toBe("");
  });
});

describe("formatting", () => {
  it("gives volts two places and dBm none", () => {
    expect(formatVolts(3.6789)).toBe("3.68 V");
    expect(formatRssi(-71.6)).toBe("-72 dBm");
  });
});

describe("sparklinePath", () => {
  it("is empty with nothing to draw", () => {
    expect(sparklinePath([], 80, 20)).toBe("");
  });
  it("spans the box, newest on the right and highest at the top", () => {
    expect(
      sparklinePath(
        [
          [0, 4.1],
          [50, 3.9],
          [100, 3.7],
        ],
        100,
        20,
      ),
    ).toBe("M0.0 0.0 L50.0 10.0 L100.0 20.0");
  });
  it("draws a flat series through the middle", () => {
    expect(
      sparklinePath(
        [
          [0, 4],
          [10, 4],
        ],
        100,
        20,
      ),
    ).toBe("M0.0 10.0 L100.0 10.0");
  });
});

describe("quietSources", () => {
  const stream = (
    stream_id: number,
    source_id: number,
    metric: string,
    stale: boolean,
    newest: number | null,
  ): Freshness => ({
    stream_id,
    source_id,
    metric,
    source_kind: "brewd_journal",
    label: source_id === 1 ? "ispindel-1" : "esp32-fv1",
    newest,
    stale,
  });

  it("gives a device gone quiet on every stream one line", () => {
    const q = quietSources(
      [stream(1, 1, "sg", true, 100), stream(2, 1, "battery_v", true, 90)],
      {},
    );
    expect(q).toEqual([
      {
        source_id: 1,
        label: "ispindel-1",
        silence: "silent",
        newest: 100,
        metrics: null,
      },
    ]);
  });
  it("names the streams when only some are quiet", () => {
    const q = quietSources(
      [
        stream(1, 2, "temp_c", false, 100),
        stream(2, 2, "room_temp_c", true, 50),
      ],
      {},
    );
    expect(q[0].metrics).toEqual(["room temp"]);
  });
  it("leaves out a source with nothing stale, and sorts by label", () => {
    const q = quietSources(
      [
        stream(1, 1, "sg", true, 1),
        stream(2, 2, "temp_c", true, 2),
        stream(3, 3, "ph", false, 3),
      ],
      { 2: status("unreachable") },
    );
    expect(q.map((s) => [s.label, s.silence])).toEqual([
      ["esp32-fv1", "unreachable"],
      ["ispindel-1", "silent"],
    ]);
  });
});
