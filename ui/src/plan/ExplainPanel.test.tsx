import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { Explanation } from "../api";
import { ExplainView } from "./ExplainPanel";
import { suggestFiles } from "./model";

const base: Explanation = {
  path: "/media/cam/clip.mp4",
  scope: { kind: "scanned", source: "cam" },
  stages: [],
  other_targets: [],
  ai: { kind: "no_candidates" },
  decision: { kind: "unmatched", target: null, via: null, detail: null },
  outcome: "unmatched",
  outputs: [],
  verdict: "Extension .mp4 is not in the match [@photo-original, @png] of target pics",
};

describe("explain view", () => {
  it("leads with the verdict and shows why each filter missed", () => {
    render(
      <ExplainView
        ex={{
          ...base,
          stages: [
            {
              stage: "before_ai",
              name: "pics",
              candidate: false,
              taken: false,
              filters: [
                {
                  index: 0,
                  sources: ["cam"],
                  source_ok: true,
                  matches: {
                    source: "@photo-original | @png",
                    expanded: "*.jpg | *.png",
                    matched: false,
                    items: [
                      { item: "@photo-original", negated: false, hit: false, expanded: "", children: [], reason: "extension .mp4 does not match" },
                      {
                        item: "@video-original",
                        negated: false,
                        hit: false,
                        expanded: "*.{mp4,m4v} & !*.av1.mp4",
                        reason: "matched *.{mp4,m4v}, but excluded by !*.av1.mp4",
                        children: [
                          { item: "*.{mp4,m4v}", negated: false, hit: true, expanded: "", children: [], reason: "matched" },
                          { item: "!*.av1.mp4", negated: true, hit: true, expanded: "", children: [], reason: "name excluded" },
                        ],
                      },
                    ],
                  },
                  exclude: null,
                  fit: "no",
                },
              ],
            },
          ],
          other_targets: ["a", "b", "c"],
        }}
      />,
    );
    expect(screen.getByText("3 more targets not for source cam")).toBeInTheDocument();
    expect(screen.getByText(/matched \*\.\{mp4,m4v\}, but excluded by !\*\.av1\.mp4/)).toBeInTheDocument();
    expect(screen.getByText(/extension \.mp4 does not match/)).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Extension .mp4 is not in the match [@photo-original, @png]");
    expect(screen.getByText("source “cam”, the scan sees it")).toBeInTheDocument();
    expect(screen.getByText("filter 1: match [@photo-original, @video-original] did not take it")).toBeInTheDocument();
    expect(screen.getByText("does not fit")).toBeInTheDocument();
    expect(screen.getByText("No target took the file")).toBeInTheDocument();
  });

  it("lists content steps with their masks and the kept destination", () => {
    render(
      <ExplainView
        ex={{
          ...base,
          path: "/media/cam/20260101_120000.png",
          outcome: "unchanged",
          verdict: "The file passes the filter of target pics, but no content step matches",
          decision: { kind: "target", target: "pics", via: { kind: "mask" }, detail: null },
          ai: { kind: "not_asked", candidates: ["cats"] },
          outputs: [
            {
              op: "keep",
              dest: "/media/cam/20260101_120000.png",
              content: [
                {
                  index: 0,
                  preset: "avif-high",
                  bundle: "compress-all",
                  mask: "@photo-original",
                  mask_expanded: "*.{jpg,jpeg,heic,heif}",
                  matched: false,
                  before: "20260101_120000.png",
                  after: "20260101_120000.png",
                  skipped: null,
                },
              ],
              rename: [
                {
                  index: 0,
                  preset: "date-name",
                  mask: "*",
                  mask_expanded: "*",
                  matched: true,
                  before: "20260101_120000.png",
                  after: "20260101_120000.png",
                  skipped: null,
                },
              ],
            },
          ],
        }}
      />,
    );
    expect(screen.getByText("Target “pics” (mask)")).toBeInTheDocument();
    expect(screen.getByText(/AI was not asked/)).toBeInTheDocument();
    const out = screen.getByText("Unchanged").closest("li")!;
    expect(within(out).getByText("mask did not match")).toBeInTheDocument();
    // A step that came from a bundle names it.
    expect(within(out).getByText("compress-all › avif-high")).toBeInTheDocument();
    expect(within(out).getByText("mask @photo-original = *.{jpg,jpeg,heic,heif}")).toBeInTheDocument();
    expect(within(out).getByText("applied, same name")).toBeInTheDocument();
  });
});

describe("file search", () => {
  const files = ["/m/a/IMG_1.jpg", "/m/b/clip.mp4", "/m/b/img_2.png", "/m/x/photo_img.jpg"];
  it("matches by words in the path, names that start with the query first", () => {
    expect(suggestFiles(files, "img")).toEqual(["/m/a/IMG_1.jpg", "/m/b/img_2.png", "/m/x/photo_img.jpg"]);
    expect(suggestFiles(files, "photo")).toEqual(["/m/x/photo_img.jpg"]);
    expect(suggestFiles(files, "b clip")).toEqual(["/m/b/clip.mp4"]);
    expect(suggestFiles(files, "  ")).toEqual([]);
    expect(suggestFiles(files, "img", 1)).toHaveLength(1);
  });
});
