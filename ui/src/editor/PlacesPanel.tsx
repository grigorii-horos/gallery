import { useState } from "react";
import { useTranslation } from "react-i18next";
import { EditorBar, EditorFailure, Field, TextInput } from "./fields";
import type { PlaceDraft } from "./model";
import type { ConfigEditor } from "./useConfigEditor";

/** "47.0245, 28.8323" (also with `;` or a space): a pasted pair of coordinates. */
export function parseLatLon(text: string): { lat: number; lon: number } | null {
  const m = /^\s*(-?\d+(?:\.\d+)?)\s*[,;\s]\s*(-?\d+(?:\.\d+)?)\s*$/.exec(text);
  return m ? { lat: Number(m[1]), lon: Number(m[2]) } : null;
}

const num = (s: string): number | null => (s.trim() !== "" && Number.isFinite(Number(s)) ? Number(s) : null);

/** Name, latitude, longitude and radius of one place; a pair pasted into a coordinate fills both. */
function PlaceFields({
  place,
  nameFixed,
  onChange,
}: {
  place: { name: string; lat: string; lon: string; km: string };
  nameFixed: boolean;
  onChange: (p: { name: string; lat: string; lon: string; km: string }) => void;
}) {
  const { t } = useTranslation();
  const coord = (k: "lat" | "lon") => (text: string) => {
    const pair = parseLatLon(text);
    onChange(pair ? { ...place, lat: String(pair.lat), lon: String(pair.lon) } : { ...place, [k]: text });
  };
  return (
    <div className="place-fields">
      {nameFixed ? <strong className="place-name">{place.name}</strong> : <TextInput label={t("place_name")} placeholder={t("place_name")} value={place.name} onChange={(name) => onChange({ ...place, name })} />}
      <TextInput mono label={t("place_lat")} placeholder={t("place_lat")} value={place.lat} onChange={coord("lat")} />
      <TextInput mono label={t("place_lon")} placeholder={t("place_lon")} value={place.lon} onChange={coord("lon")} />
      <TextInput mono label={t("place_km")} placeholder={t("place_km")} value={place.km} onChange={(km) => onChange({ ...place, km })} />
    </div>
  );
}

const toText = (p: PlaceDraft) => ({ name: p.name, lat: String(p.lat), lon: String(p.lon), km: String(p.km) });

function PlaceRow({ place, editor }: { place: PlaceDraft; editor: ConfigEditor }) {
  const { t } = useTranslation();
  const [form, setForm] = useState(toText(place));
  return (
    <li className="place-row">
      <PlaceFields
        place={form}
        nameFixed
        onChange={(f) => {
          setForm(f);
          const lat = num(f.lat);
          const lon = num(f.lon);
          const km = num(f.km);
          if (lat !== null && lon !== null && km !== null) editor.edit({ op: "set_place", name: place.name, place: { lat, lon, km } });
        }}
      />
      <button type="button" className="link small danger" aria-label={t("place_remove", { name: place.name })} onClick={() => void editor.editNow({ op: "remove_place", name: place.name })}>
        {t("place_remove_short")}
      </button>
    </li>
  );
}

/// Settings → Places: the named points of `[places]` in the config file.
export function PlacesPanel({ editor }: { editor: ConfigEditor }) {
  const { t } = useTranslation();
  const [fresh, setFresh] = useState({ name: "", lat: "", lon: "", km: "1" });
  const places = editor.view?.draft.places ?? [];
  const lat = num(fresh.lat);
  const lon = num(fresh.lon);
  const km = num(fresh.km);
  const name = fresh.name.trim();
  const ready = name !== "" && lat !== null && lon !== null && km !== null;
  const add = async () => {
    if (!ready) return;
    const o = await editor.editNow({ op: "set_place", name, place: { lat, lon, km } });
    if (o) setFresh({ name: "", lat: "", lon: "", km: "1" });
  };
  if (!editor.view) return <EditorFailure editor={editor} />;
  return (
    <section className="places" aria-label={t("places_title")}>
      <h3>{t("places_title")}</h3>
      <p className="muted">{t("places_hint")}</p>
      <EditorFailure editor={editor} />
      {places.length === 0 && <p className="empty small">{t("places_none")}</p>}
      <ul className="place-list">
        {places.map((p) => (
          <PlaceRow key={`${p.name}-${editor.formGen}`} place={p} editor={editor} />
        ))}
      </ul>
      <Field label={t("places_add")}>
        <PlaceFields place={fresh} nameFixed={false} onChange={setFresh} />
        <button type="button" className="btn" disabled={!ready} onClick={() => void add()}>
          {t("places_add")}
        </button>
      </Field>
      <EditorBar editor={editor} />
    </section>
  );
}
