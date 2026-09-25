package com.navsdk.demo

import android.content.res.AssetManager
import com.navsdk.core.GeoPoint
import com.navsdk.core.RawLocation
import com.navsdk.core.Route
import com.navsdk.core.routeFromJson
import org.json.JSONObject

/** A route plus the simulated GPS trace recorded over it (see `fixtures/`). */
data class Fixture(val name: String, val route: Route, val trace: List<RawLocation>) {
    companion object {
        val NAMES = listOf("route_simple", "route_parallel_roads", "route_uturn", "route_detour")

        fun load(assets: AssetManager, name: String): Fixture {
            val json = JSONObject(assets.open("$name.json").bufferedReader().use { it.readText() })
            val route = routeFromJson(json.getJSONObject("route").toString())
            val traceJson = json.getJSONArray("trace")
            val trace = (0 until traceJson.length()).map { i ->
                val fix = traceJson.getJSONObject(i)
                val point = fix.getJSONObject("point")
                RawLocation(
                    point = GeoPoint(point.getDouble("lat"), point.getDouble("lng")),
                    accuracyM = fix.optDouble("accuracy_m").takeIf { !it.isNaN() },
                    speedMps = fix.optDouble("speed_mps").takeIf { !it.isNaN() },
                    courseDeg = fix.optDouble("course_deg").takeIf { !it.isNaN() },
                    timestampMs = fix.getLong("timestamp_ms").toULong(),
                )
            }
            return Fixture(name, route, trace)
        }
    }
}
