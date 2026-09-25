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

            TripCard(state, stepCount, reroutes)

            lastError?.let {
                Text("last rejected fix: $it", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelSmall)
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
            Text("Remaining: ${formatDistance(state.distanceRemainingM)}")
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
