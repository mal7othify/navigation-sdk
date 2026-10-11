package com.navsdk.demo

import android.annotation.SuppressLint
import android.app.Application
import android.location.Location
import android.os.Looper
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.google.android.gms.location.LocationCallback
import com.google.android.gms.location.LocationRequest
import com.google.android.gms.location.LocationResult
import com.google.android.gms.location.LocationServices
import com.google.android.gms.location.Priority
import com.navsdk.NavigationSession
import com.navsdk.push.NavigationSessionPush
import com.navsdk.push.core.PlannedDeviation
import com.navsdk.push.core.RouteSimulator
import com.navsdk.push.core.SimulatorConfig
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import com.navsdk.core.GeoPoint as PullGeoPoint
import com.navsdk.core.PositionState as PullPosition
import com.navsdk.core.RerouteRequest as PullRerouteRequest
import com.navsdk.core.Route as PullRoute
import com.navsdk.core.TripState as PullTrip
import com.navsdk.core.directRoute as pullDirectRoute
import com.navsdk.core.walkCourse as pullWalkCourse
import com.navsdk.push.core.GeoPoint as PushGeoPoint
import com.navsdk.push.core.PositionState as PushPosition
import com.navsdk.push.core.RerouteRequest as PushRerouteRequest
import com.navsdk.push.core.Route as PushRoute
import com.navsdk.push.core.TripState as PushTrip
import com.navsdk.push.core.directRoute as pushDirectRoute
import com.navsdk.push.core.walkCourse as pushWalkCourse

/** Which implementation the screen is driving. */
enum class Approach(val label: String) { PULL("Pull · UniFFI"), PUSH("Push · BoltFFI"), BOTH("Both") }

/** Where fixes come from. */
enum class Source(val label: String) { REPLAY("Replay fixture"), SIMULATOR("Rust simulator"), DEVICE("Device") }

/** A route in both binding layers' types, so one feed can drive both sessions. */
data class DualRoute(val pull: PullRoute, val push: PushRoute) {
    /** Geometry as plain coordinates, for the schematic map. */
    val points: List<Pair<Double, Double>> get() = pull.geometry.map { it.lat to it.lng }
    val stepRanges: List<IntRange> get() = pull.steps.map { it.startIndex.toInt()..it.endIndex.toInt() }
    val destination: PullGeoPoint get() = pull.geometry.last()
}

private const val METRICS_WINDOW = 1024
private const val REPLAN_LATENCY_MS = 300L
private const val DIVERGENCE_THRESHOLD_M = 0.5

/**
 * Drives one or two sessions over the same fixes and aggregates the metrics
 * the comparison needs. Pull and push sessions are always created together
 * so the "Both" tab can be switched to without losing state.
 *
 * Device mode can lay out a walking course at the phone's position
 * (`walkCourse`) so the navigator can be tested on foot with no map service;
 * while a course is active, each reroute request is answered with a straight
 * `directRoute` back to the destination (host-driven for pull, core-driven
 * for push). On the placeholder fixture route the device is nowhere near the
 * route, so no reroute is attempted.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class DemoViewModel(app: Application) : AndroidViewModel(app) {
    private val pull = MutableStateFlow<NavigationSession?>(null)
    private val push = MutableStateFlow<NavigationSessionPush?>(null)
    private var fixture: Fixture? = null
    private var simulator: RouteSimulator? = null
    private var feedJob: Job? = null

    val approach = MutableStateFlow(Approach.PULL)
    val source = MutableStateFlow(Source.REPLAY)
    val fixtureName = MutableStateFlow(Fixture.NAMES.first())
    val running = MutableStateFlow(false)
    val feedIndex = MutableStateFlow(0)
    val feedTotal = MutableStateFlow(0)
    val lastError = MutableStateFlow<String?>(null)
    val simulatorDeviating = MutableStateFlow(false)

    /** Route the sessions are navigating (the last installed one), for the schematic map. */
    val activeRoute = MutableStateFlow<DualRoute?>(null)
    /** Most recent raw fix handed to the core (replayed, simulated or from the device). */
    val lastRawPoint = MutableStateFlow<Pair<Double, Double>?>(null)
    /** Most recent device fix, for anchoring a walking course. */
    val lastDeviceLocation = MutableStateFlow<Location?>(null)
    /** Leg length of the walking course, metres. */
    val courseLegM = MutableStateFlow(50.0)
    /** Answer reroute requests with a straight line to the destination (walking course only). */
    val autoReroute = MutableStateFlow(true)
    /** True while the sessions navigate a walking course started from the device position. */
    val courseActive = MutableStateFlow(false)

    // ---- Pull (Approach A)
    val pullPosition: StateFlow<PullPosition?> = pull
        .flatMapLatest { it?.position ?: flowOf(null) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)
    val pullTrip: StateFlow<PullTrip?> = pull
        .flatMapLatest { it?.trip ?: flowOf(null) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)
    private val pullSamples = ArrayDeque<Long>()
    private val _pullMetrics = MutableStateFlow(ApproachMetrics())
    val pullMetrics: StateFlow<ApproachMetrics> = _pullMetrics.asStateFlow()

    // ---- Push (Approach B)
    val pushPosition: StateFlow<PushPosition?> = push
        .flatMapLatest { it?.position ?: flowOf(null) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)
    val pushTrip: StateFlow<PushTrip?> = push
        .flatMapLatest { it?.trip ?: flowOf(null) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)
    private val pushSamples = ArrayDeque<Long>()
    private val _pushMetrics = MutableStateFlow(ApproachMetrics())
    val pushMetrics: StateFlow<ApproachMetrics> = _pushMetrics.asStateFlow()

    /** Both mode: metres between the two snapped positions, and whether they ever diverged. */
    val divergenceM: StateFlow<Double?> = combine(pullPosition, pushPosition) { a, b ->
        if (a == null || b == null) null
        else haversineM(a.snapped.point.lat, a.snapped.point.lng, b.snapped.point.lat, b.snapped.point.lng)
    }.stateIn(viewModelScope, SharingStarted.Eagerly, null)
    val everDiverged = MutableStateFlow(false)

    private val fused = LocationServices.getFusedLocationProviderClient(app)
    private val locationCallback = object : LocationCallback() {
        override fun onLocationResult(result: LocationResult) {
            val location = result.lastLocation ?: return
            lastDeviceLocation.value = location
            lastRawPoint.value = location.latitude to location.longitude
            if (approach.value != Approach.PUSH) pull.value?.update(location)
            if (approach.value != Approach.PULL) push.value?.update(location)
        }
    }

    init {
        viewModelScope.launch {
            divergenceM.collect { d -> if (d != null && d > DIVERGENCE_THRESHOLD_M) everDiverged.value = true }
        }
        selectFixture(fixtureName.value)
    }

    fun selectApproach(next: Approach) {
        approach.value = next
    }

    fun selectFixture(name: String) {
        stopFeed()
        fixtureName.value = name
        viewModelScope.launch {
            val loaded = withContext(Dispatchers.IO) { Fixture.load(getApplication<Application>().assets, name) }
            fixture = loaded
            feedTotal.value = loaded.size
            feedIndex.value = 0
            startSessions(DualRoute(loaded.pullRoute, loaded.pushRoute), course = false)
        }
    }

    /** Fresh sessions and metrics for both approaches on [route]. */
    private fun startSessions(route: DualRoute, course: Boolean) {
        pull.value?.close()
        push.value?.close()
        simulator?.close()
        simulator = RouteSimulator(route.push, SimulatorConfig())
        simulatorDeviating.value = false
        everDiverged.value = false
        courseActive.value = course
        activeRoute.value = route
        lastRawPoint.value = null
        lastError.value = null
        pullSamples.clear()
        pushSamples.clear()
        _pullMetrics.value = ApproachMetrics()
        _pushMetrics.value = ApproachMetrics()

        // Pull: host-driven. The wrapper runs this closure and installs the result.
        val a = NavigationSession(route.pull, replanner = { request -> replanPull(route, request) })
        // Push: core-driven. Rust calls this from its helper thread and installs the result itself.
        val b = NavigationSessionPush(route.push, replanner = { request -> replanPush(route, request) })
        pull.value = a
        push.value = b

        viewModelScope.launch { a.errors.collect { lastError.value = "pull: ${it.message}" } }
        viewModelScope.launch { b.errors.collect { lastError.value = "push: ${it.message}" } }
        viewModelScope.launch { a.callNanos.collect { pullSamples.push(it); refreshPull(a) } }
        viewModelScope.launch { b.callNanos.collect { pushSamples.push(it); refreshPush(b) } }
        viewModelScope.launch {
            combine(a.positionUpdates, a.tripUpdates, a.rerouteRequests, a.routesInstalled) { _, _, _, _ -> }
                .collect { refreshPull(a) }
        }
        viewModelScope.launch {
            combine(b.positionCallbacks, b.tripCallbacks, b.rerouteRequests, b.session) { _, _, _, _ -> }
                .collect { refreshPush(b) }
        }
    }

    /**
     * The demo has no routing backend. Replaying or simulating a fixture, the
     * "fetched" route is the same route after a short delay. On a walking
     * course it is a straight line from the device to the destination. On a
     * fixture route in Device mode the phone is nowhere near it: decline.
     */
    private suspend fun replanPull(route: DualRoute, request: PullRerouteRequest): PullRoute? {
        delay(REPLAN_LATENCY_MS)
        if (source.value != Source.DEVICE) return route.pull
        if (!autoReroute.value || !courseActive.value) return null
        return runCatching {
            withContext(Dispatchers.Default) { pullDirectRoute(request.origin, route.destination) }
        }.onFailure { lastError.value = "reroute failed: ${it.message}" }.getOrNull()?.also { installed ->
            activeRoute.value = DualRoute(installed, toPush(installed))
        }
    }

    private suspend fun replanPush(route: DualRoute, request: PushRerouteRequest): PushRoute? {
        delay(REPLAN_LATENCY_MS)
        if (source.value != Source.DEVICE) return route.push
        if (!autoReroute.value || !courseActive.value) return null
        val to = PushGeoPoint(route.destination.lat, route.destination.lng)
        return runCatching {
            withContext(Dispatchers.Default) { pushDirectRoute(request.origin, to) }
        }.onFailure { lastError.value = "reroute failed: ${it.message}" }.getOrNull()?.also { installed ->
            activeRoute.value = DualRoute(toPull(installed), installed)
        }
    }

    /**
     * Lay out a zigzag walking course at the current device position, facing
     * the way the device is moving (north if it has no bearing yet), and
     * navigate it with both sessions.
     */
    fun startWalkCourse() {
        val here = lastDeviceLocation.value ?: return
        val heading = if (here.hasBearing()) here.bearing.toDouble() else 0.0
        stopFeed()
        try {
            val a = pullWalkCourse(PullGeoPoint(here.latitude, here.longitude), heading, courseLegM.value)
            val b = pushWalkCourse(PushGeoPoint(here.latitude, here.longitude), heading, courseLegM.value)
            startSessions(DualRoute(a, b), course = true)
        } catch (e: Exception) {
            lastError.value = e.message
        }
    }

    private fun ArrayDeque<Long>.push(sample: Long) {
        addLast(sample)
        if (size > METRICS_WINDOW) removeFirst()
    }

    private fun refreshPull(a: NavigationSession) {
        _pullMetrics.update {
            ApproachMetrics(
                boundary = BoundaryStats.of(pullSamples.toList(), a.positionUpdates.value),
                positionUpdates = a.positionUpdates.value,
                tripUpdates = a.tripUpdates.value,
                rerouteRequests = a.rerouteRequests.value,
                routesInstalled = a.routesInstalled.value,
            )
        }
    }

    private fun refreshPush(b: NavigationSessionPush) {
        val session = b.session.value
        _pushMetrics.update {
            ApproachMetrics(
                boundary = BoundaryStats.of(pushSamples.toList(), b.positionCallbacks.value),
                positionUpdates = b.positionCallbacks.value,
                tripUpdates = b.tripCallbacks.value,
                rerouteRequests = b.rerouteRequests.value,
                routesInstalled = session.routesInstalled.toInt(),
                replanInFlight = session.replanInFlight,
            )
        }
    }

    /** Feed the chosen source at 1 Hz × [speed] to the session(s) of the current tab. */
    fun start(speed: Double = 1.0) {
        val loaded = fixture ?: return
        stopFeed()
        startSessions(DualRoute(loaded.pullRoute, loaded.pushRoute), course = false)
        running.value = true
        val tab = approach.value
        feedJob = viewModelScope.launch {
            when (source.value) {
                Source.REPLAY -> {
                    for (i in 0 until loaded.size) {
                        lastRawPoint.value = loaded.pullTrace[i].point.let { it.lat to it.lng }
                        feedOne(tab, loaded, i)
                        feedIndex.value = i + 1
                        delay((1000.0 / speed).toLong())
                    }
                }
                Source.SIMULATOR -> {
                    val sim = simulator ?: return@launch
                    var n = 0
                    var tail = 5
                    feedTotal.value = 0
                    while (tail > 0) {
                        val fix = sim.nextFix()
                        lastRawPoint.value = fix.point.lat to fix.point.lng
                        if (tab != Approach.PUSH) pull.value?.update(fix.toPull())
                        if (tab != Approach.PULL) push.value?.update(fix)
                        feedIndex.value = ++n
                        if (sim.isFinished()) tail -= 1
                        delay((1000.0 / speed).toLong())
                    }
                }
                Source.DEVICE -> return@launch
            }
            running.value = false
        }
    }

    private suspend fun feedOne(tab: Approach, loaded: Fixture, i: Int) {
        when (tab) {
            Approach.PULL -> pull.value?.update(loaded.pullTrace[i])
            Approach.PUSH -> push.value?.update(loaded.pushTrace[i])
            // Lockstep: wait for each so the divergence check compares the same fix.
            Approach.BOTH -> {
                runCatching { pull.value?.updateAndAwait(loaded.pullTrace[i]) }
                push.value?.updateAndAwait(loaded.pushTrace[i])
            }
        }
    }

    /** Simulator only: leave the route 50 m ahead for 300 m, 80 m to the right. */
    fun deviateNow() {
        val sim = simulator ?: return
        sim.setPlannedDeviation(PlannedDeviation(atM = sim.distanceAlongM() + 50.0, lateralM = 80.0, lengthM = 300.0))
        simulatorDeviating.value = true
    }

    fun stopFeed() {
        feedJob?.cancel()
        feedJob = null
        running.value = false
    }

    /** Switch the fix source. Device location needs the permission granted first. */
    @SuppressLint("MissingPermission") // The Activity requests permission before calling.
    fun setSource(next: Source) {
        if (source.value == next) return
        source.value = next
        stopFeed()
        fixture?.let { startSessions(DualRoute(it.pullRoute, it.pushRoute), course = false) }
        if (next == Source.DEVICE) {
            val request = LocationRequest.Builder(Priority.PRIORITY_HIGH_ACCURACY, 1000L).build()
            fused.requestLocationUpdates(request, locationCallback, Looper.getMainLooper())
        } else {
            fused.removeLocationUpdates(locationCallback)
        }
    }

    override fun onCleared() {
        stopFeed()
        fused.removeLocationUpdates(locationCallback)
        pull.value?.close()
        push.value?.close()
        simulator?.close()
    }
}

/** The same route in the other binding layer's type. */
private fun toPush(r: PullRoute): PushRoute = PushRoute(
    geometry = r.geometry.map { PushGeoPoint(it.lat, it.lng) },
    steps = r.steps.map {
        com.navsdk.push.core.RouteStep(
            it.instruction, com.navsdk.push.core.ManeuverType.valueOf(it.maneuver.name),
            it.startIndex, it.endIndex, it.distanceM,
        )
    },
)

private fun toPull(r: PushRoute): PullRoute = PullRoute(
    geometry = r.geometry.map { PullGeoPoint(it.lat, it.lng) },
    steps = r.steps.map {
        com.navsdk.core.RouteStep(
            it.instruction, com.navsdk.core.ManeuverType.valueOf(it.maneuver.name),
            it.startIndex, it.endIndex, it.distanceM,
        )
    },
)
