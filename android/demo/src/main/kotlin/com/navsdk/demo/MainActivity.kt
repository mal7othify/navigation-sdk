package com.navsdk.demo

import android.Manifest
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.sp
import com.navsdk.core.GeoPoint
import com.navsdk.core.ManeuverType
import com.navsdk.core.Route
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.FilterChip
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.navsdk.core.TripProgress
import com.navsdk.core.TripState
import com.navsdk.core.coreVersion
import java.util.Locale

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                DemoScreen()
            }
        }
    }
}

@Composable
fun DemoScreen(vm: DemoViewModel = viewModel()) {
    val state by vm.state.collectAsStateWithLifecycle()
    val fixtureName by vm.fixtureName.collectAsStateWithLifecycle()
    val source by vm.source.collectAsStateWithLifecycle()
    val replaying by vm.replaying.collectAsStateWithLifecycle()
    val replayIndex by vm.replayIndex.collectAsStateWithLifecycle()
    val replayTotal by vm.replayTotal.collectAsStateWithLifecycle()
    val stepCount by vm.stepCount.collectAsStateWithLifecycle()
    val reroutes by vm.rerouteRequests.collectAsStateWithLifecycle()
    val lastError by vm.lastError.collectAsStateWithLifecycle()
    val activeRoute by vm.activeRoute.collectAsStateWithLifecycle()
    val lastRawPoint by vm.lastRawPoint.collectAsStateWithLifecycle()
    val lastDeviceLocation by vm.lastDeviceLocation.collectAsStateWithLifecycle()
    val courseLegM by vm.courseLegM.collectAsStateWithLifecycle()
    val autoReroute by vm.autoReroute.collectAsStateWithLifecycle()
    val reroutesInstalled by vm.reroutesInstalled.collectAsStateWithLifecycle()

    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) vm.setSource(Source.DEVICE)
    }

    Scaffold { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("NavSDK demo · core ${coreVersion()}", style = MaterialTheme.typography.labelMedium)

            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Use device location", modifier = Modifier.width(180.dp))
                Switch(
                    checked = source == Source.DEVICE,
                    onCheckedChange = { on ->
                        if (on) permission.launch(Manifest.permission.ACCESS_FINE_LOCATION)
                        else vm.setSource(Source.REPLAY)
                    },
                )
            }

            if (source == Source.REPLAY) {
                LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    items(Fixture.NAMES) { name ->
                        FilterChip(
                            selected = name == fixtureName,
                            onClick = { vm.selectFixture(name) },
                            label = { Text(name.removePrefix("route_")) },
                        )
                    }
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { vm.startReplay() }, enabled = !replaying) { Text("Replay 1×") }
                    Button(onClick = { vm.startReplay(10.0) }, enabled = !replaying) { Text("Replay 10×") }
                    OutlinedButton(onClick = { vm.stopReplay() }, enabled = replaying) { Text("Stop") }
                }
                if (replayTotal > 0) {
                    LinearProgressIndicator(
                        progress = { replayIndex.toFloat() / replayTotal },
                        modifier = Modifier.fillMaxWidth(),
                    )
                    Text("fix $replayIndex / $replayTotal", style = MaterialTheme.typography.labelSmall)
                }
            }

            if (source == Source.DEVICE) {
                val loc = lastDeviceLocation
                Text(
                    if (loc == null) "Waiting for a GPS fix…"
                    else String.format(
                        Locale.US, "GPS %.5f, %.5f · ±%.0f m · %s",
                        loc.latitude, loc.longitude, loc.accuracy,
                        if (loc.hasBearing()) String.format(Locale.US, "bearing %.0f°", loc.bearing) else "no bearing yet",
                    ),
                    style = MaterialTheme.typography.bodySmall,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf(30.0, 50.0, 100.0).forEach { leg ->
                        FilterChip(
                            selected = courseLegM == leg,
                            onClick = { vm.courseLegM.value = leg },
                            label = { Text("${leg.toInt()} m") },
                        )
                    }
                }
                Button(onClick = { vm.startWalkCourse() }, enabled = loc != null) { Text("Start walking course here") }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Auto-reroute when the core asks", modifier = Modifier.width(220.dp))
                    Switch(checked = autoReroute, onCheckedChange = { vm.autoReroute.value = it })
                }
                Text(
                    "Walk straight, turn right, turn left. You finish about two legs from where you started; " +
                        "walk back and tap again to repeat. Leave the course on purpose to see OFF ROUTE and a reroute.",
                    style = MaterialTheme.typography.labelSmall,
                )
                if (reroutesInstalled > 0) {
                    Text("reroutes installed: $reroutesInstalled", style = MaterialTheme.typography.labelSmall)
                }
            }

            TripCard(state, stepCount, reroutes)

            activeRoute?.let { route ->
                CourseMap(route = route, state = state, rawPoint = lastRawPoint, modifier = Modifier.fillMaxWidth().height(280.dp))
            }

            lastError?.let {
                Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}

@Composable
private fun TripCard(state: TripState?, stepCount: Int, reroutes: Int) {
    Card(modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (state == null) {
                Text("Waiting for the first fix…", style = MaterialTheme.typography.titleMedium)
                return@Column
            }
            Text(state.nextInstruction, style = MaterialTheme.typography.headlineSmall)
            Text("in ${formatDistance(state.distanceToNextManeuverM)}", style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.height(4.dp))
            Text("Remaining: ${formatDistance(state.distanceRemainingM)} · ETA ${formatEta(state.etaS)}")
            Text(String.format(Locale.US, "Speed %.1f km/h", state.speedMps * 3.6))
            Text("Step ${state.currentStepIndex.toInt() + 1} of $stepCount")
            Text(
                String.format(
                    Locale.US,
                    "Snapped %.5f, %.5f · %.0f m from route · heading %.0f°",
                    state.snapped.point.lat,
                    state.snapped.point.lng,
                    state.snapped.distanceFromRouteM,
                    state.snapped.bearingDeg,
                ),
                style = MaterialTheme.typography.bodySmall,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                if (state.isOffRoute) Badge("OFF ROUTE", Color(0xFFB00020))
                if (state.progress == TripProgress.ARRIVED) Badge("ARRIVED", Color(0xFF2E7D32))
                if (reroutes > 0) Badge("reroutes requested: $reroutes", Color(0xFF6D4C41))
            }
        }
    }
}

@Composable
private fun Badge(text: String, color: Color) {
    AssistChip(onClick = {}, label = { Text(text, color = Color.White) }, colors = androidx.compose.material3.AssistChipDefaults.assistChipColors(containerColor = color))
}

private fun formatDistance(m: Double): String =
    if (m >= 1000) String.format(Locale.US, "%.1f km", m / 1000) else String.format(Locale.US, "%.0f m", m)

/** `null` until the core has seen a few seconds of movement. */
private fun formatEta(s: Double?): String = when {
    s == null -> "—"
    s < 60 -> String.format(Locale.US, "%.0f s", s)
    s < 3600 -> String.format(Locale.US, "%.0f min", s / 60)
    else -> String.format(Locale.US, "%.0f h %02.0f min", Math.floor(s / 3600), (s / 60) % 60)
}

/**
 * Schematic map: the route in a local metres frame, the current step
 * highlighted, the raw fix and the snapped position. No map tiles needed.
 */
@Composable
private fun CourseMap(route: Route, state: TripState?, rawPoint: GeoPoint?, modifier: Modifier = Modifier) {
    val measurer = rememberTextMeasurer()
    val labelStyle = TextStyle(fontSize = 10.sp, color = MaterialTheme.colorScheme.onSurface)
    val fg = MaterialTheme.colorScheme.onSurface
    Canvas(modifier = modifier.clip(RoundedCornerShape(12.dp)).background(MaterialTheme.colorScheme.surfaceVariant)) {
        val pts = route.geometry
        val origin = pts.firstOrNull() ?: return@Canvas
        val mPerDegLat = 110_540.0
        val mPerDegLng = 111_320.0 * Math.cos(Math.toRadians(origin.lat))
        fun enu(p: GeoPoint) = Offset(((p.lng - origin.lng) * mPerDegLng).toFloat(), ((p.lat - origin.lat) * mPerDegLat).toFloat())
        val all = pts.map(::enu).toMutableList()
        rawPoint?.let { all += enu(it) }
        state?.let { all += enu(it.snapped.point) }
        val minX = all.minOf { it.x }; val maxX = all.maxOf { it.x }
        val minY = all.minOf { it.y }; val maxY = all.maxOf { it.y }
        val span = maxOf(maxX - minX, maxY - minY, 20f)
        val pad = 24.dp.toPx()
        val k = (minOf(size.width, size.height) - 2 * pad) / span
        val cx = (minX + maxX) / 2; val cy = (minY + maxY) / 2
        fun screen(e: Offset) = Offset(size.width / 2 + (e.x - cx) * k, size.height / 2 - (e.y - cy) * k)
        fun polyline(points: List<Offset>) = Path().apply {
            points.forEachIndexed { i, p -> if (i == 0) moveTo(p.x, p.y) else lineTo(p.x, p.y) }
        }

        // Whole route.
        drawPath(polyline(pts.map { screen(enu(it)) }), Color.Gray.copy(alpha = 0.5f), style = Stroke(3.dp.toPx(), join = StrokeJoin.Round))

        // Current step.
        if (state != null) {
            val step = route.steps[state.currentStepIndex.toInt()]
            val lo = step.startIndex.toInt(); val hi = step.endIndex.toInt()
            if (hi > lo) {
                drawPath(polyline(pts.subList(lo, hi + 1).map { screen(enu(it)) }), Color(0xFF1D5BBF), style = Stroke(5.dp.toPx(), join = StrokeJoin.Round))
            }
        }

        // Manoeuvre points and destination.
        for (step in route.steps) {
            val p = screen(enu(pts[step.startIndex.toInt()]))
            if (step.maneuver == ManeuverType.ARRIVE) {
                val r = 7.dp.toPx()
                drawRect(Color(0xFF2E7D32), topLeft = Offset(p.x - r, p.y - r), size = Size(2 * r, 2 * r))
            } else {
                drawCircle(Color.White, 5.dp.toPx(), p)
                drawCircle(Color.Gray, 5.dp.toPx(), p, style = Stroke(2.dp.toPx()))
            }
        }

        // Raw fix (hollow red) → snapped (filled blue; red when off route).
        rawPoint?.let { raw ->
            val p = screen(enu(raw))
            drawCircle(Color(0xFFB00020), 6.dp.toPx(), p, style = Stroke(2.dp.toPx()))
            state?.let {
                val q = screen(enu(it.snapped.point))
                drawLine(Color(0x99B00020), p, q, strokeWidth = 1.dp.toPx(), pathEffect = PathEffect.dashPathEffect(floatArrayOf(6f, 6f)))
            }
        }
        state?.let {
            drawCircle(if (it.isOffRoute) Color(0xFFB00020) else Color(0xFF1D5BBF), 7.dp.toPx(), screen(enu(it.snapped.point)))
        }

        // North arrow and 10 m scale bar.
        drawText(measurer, "N", Offset(size.width - 18.dp.toPx(), 4.dp.toPx()), labelStyle)
        drawLine(fg, Offset(size.width - 14.dp.toPx(), 32.dp.toPx()), Offset(size.width - 14.dp.toPx(), 20.dp.toPx()), strokeWidth = 1.5.dp.toPx(), cap = StrokeCap.Round)
        val barY = size.height - 12.dp.toPx()
        drawLine(fg, Offset(12.dp.toPx(), barY), Offset(12.dp.toPx() + 10 * k, barY), strokeWidth = 2.dp.toPx())
        drawText(measurer, "10 m", Offset(12.dp.toPx(), barY - 16.dp.toPx()), labelStyle)
    }
}
