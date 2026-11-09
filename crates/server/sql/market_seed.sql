-- NPC sell orders for every published market type at every starter station.
-- Price per unit: the retail Serenity average price (game.retail_prices), else
-- the SDE base price, else the base value of its reprocessing materials, else a
-- flat 10,000 ISK (SKINs, apparel and the like have neither).
-- Bind $1 to TYPE_PLEX and $2 to the captured UTC-day FILETIME.
WITH starter AS (
    SELECT DISTINCT st.station_id::bigint, st.solar_system_id::bigint, st.constellation_id::bigint, st.region_id::bigint
    FROM sde_native.schools s, jsonb_array_elements_text(s.data->'startingStations') x(id)
    JOIN sde.sta_stations st ON st.station_id = x.id::int
), materials AS (
    SELECT m.type_id, sum(m.quantity * mt.base_price / GREATEST(mt.portion_size, 1)) / GREATEST(max(t.portion_size), 1) AS value
    FROM sde.inv_type_materials m
    JOIN sde.inv_types mt ON mt.type_id = m.material_type_id
    JOIN sde.inv_types t ON t.type_id = m.type_id
    GROUP BY m.type_id
), priced AS (
    SELECT t.type_id,
           GREATEST(round(COALESCE(NULLIF(r.average, 0), NULLIF(t.base_price / GREATEST(t.portion_size, 1), 0), NULLIF(m.value, 0), 10000)::numeric, 2), 0.01) AS price
    FROM sde.inv_types t LEFT JOIN materials m USING (type_id) LEFT JOIN game.retail_prices r USING (type_id)
    WHERE t.published AND t.market_group_id IS NOT NULL AND t.type_id <> $1::integer
), changed AS (
    INSERT INTO game.market_orders (type_id, station_id, solar_system_id, constellation_id, region_id,
                                    price, vol_entered, vol_remaining, bid, duration, issued)
    SELECT p.type_id, s.station_id, s.solar_system_id, s.constellation_id, s.region_id, p.price, 1000000, 1000000, false, 365, $2::bigint
    FROM priced p CROSS JOIN starter s
    RETURNING market_scope, type_id, region_id
)
SELECT * FROM changed
