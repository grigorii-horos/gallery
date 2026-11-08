import { planApi } from "../api";
import { ThumbQueue } from "./thumbQueue";

// One queue for the app's lifetime: thumbnails stay cached across screens.
export const thumbQueue = new ThumbQueue((p) => planApi.thumb(p), 6);
