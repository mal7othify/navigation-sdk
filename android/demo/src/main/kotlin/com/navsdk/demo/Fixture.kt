package com.navsdk.demo

import android.content.res.AssetManager
import org.json.JSONObject
import com.navsdk.core.GeoPoint as PullGeoPoint
import com.navsdk.core.RawLocation as PullRawLocation
import com.navsdk.core.Route as PullRoute
import com.navsdk.core.routeFromJson as pullRouteFromJson
import com.navsdk.push.core.GeoPoint as PushGeoPoint
import com.navsdk.push.core.RawLocation as PushRawLocation
import com.navsdk.push.core.Route as PushRoute
import com.navsdk.push.core.routeFromJson as pushRouteFromJson

/**
 * A route plus the simulated GPS trace recorded over it (see `fixtures/`),
 * decoded once into the types of **both** binding layers so the same drive
 * can be fed to the pull (UniFFI) and push (BoltFFI) sessions.
 */
data class Fixture(
    val name: String,
    val pullRoute: PullRoute,
    val pullTrace: List<PullRawLocation>,
    val pushRoute: PushRoute,
    val pushTrace: List<PushRawLocation>,
) {
    val size: Int get() = pullTrace.size

    companion object {
        val NAMES = listOf("route_simple", "route_parallel_roads", "route_uturn", "route_detour")

        fun load(assets: AssetManager, name: String): Fixture {
            val json = JSONObject(assets.open("$name.json").bufferedReader().use { it.readText() })
            val routeJson = json.getJSONObject("route").toString()
            val traceJson = json.getJSONArray("trace")
            val pull = ArrayList<PullRawLocation>(traceJson.length())
            val push = ArrayList<PushRawLocation>(traceJson.length())
            for (i in 0 until traceJson.length()) {
                val fix = traceJson.getJSONObject(i)
                val point = fix.getJSONObject("point")
                val lat = point.getDouble("lat")
                val lng = point.getDouble("lng")
                val accuracy = fix.optDouble("accuracy_m").takeIf { !it.isNaN() }
                val speed = fix.optDouble("speed_mps").takeIf { !it.isNaN() }
                val course = fix.optDouble("course_deg").takeIf { !it.isNaN() }
                val ts = fix.getLong("timestamp_ms").toULong()
                pull += PullRawLocation(PullGeoPoint(lat, lng), accuracy, speed, course, ts)
                push += PushRawLocation(PushGeoPoint(lat, lng), accuracy, speed, course, ts)
            }
            return Fixture(
                name = name,
                pullRoute = pullRouteFromJson(routeJson),
                pullTrace = pull,
                pushRoute = pushRouteFromJson(routeJson),
                pushTrace = push,
            )
        }
    }
}

/** The same fix in the other binding layer's type. */
fun PushRawLocation.toPull(): PullRawLocation =
    PullRawLocation(PullGeoPoint(point.lat, point.lng), accuracyM, speedMps, courseDeg, timestampMs)
