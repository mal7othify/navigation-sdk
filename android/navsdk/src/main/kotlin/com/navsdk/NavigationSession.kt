package com.navsdk

import android.location.Location
import com.navsdk.core.FullState
import com.navsdk.core.GeoPoint
import com.navsdk.core.NavException
import com.navsdk.core.Navigator
import com.navsdk.core.NavigatorConfig
import com.navsdk.core.PositionState
import com.navsdk.core.RawLocation
import com.navsdk.core.RerouteReason
import com.navsdk.core.RerouteRequest
import com.navsdk.core.Route
import com.navsdk.core.TripState
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * Approach A, **pull**: idiomatic wrapper over the UniFFI [Navigator].
 *
 * Every fix is a synchronous call into Rust on [dispatcher] (never the main
 * thread) that returns the whole [FullState]. The wrapper splits it into
 * [position] (every fix) and [trip] (only when the core says it changed), so
 * UI bound to [trip] recomposes a handful of times per drive.
 *
 * Rerouting is host-driven: the core reports a [RerouteRequest] inside the
 * trip state, this wrapper surfaces it on [rerouteEvents] and, when a
 * [replanner] is supplied, fetches the new route and installs it with
 * [setRoute]. Rejected fixes (stale, implausible, invalid) surface on
 * [errors] and leave the state untouched.
 *
 * Call [close] when done to release the native object.
 */
class NavigationSession(
    route: Route,
    config: NavigatorConfig = NavigatorConfig(),
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
    private val replanner: (suspend (RerouteRequest) -> Route?)? = null,
) : AutoCloseable {
    private val navigator = Navigator(route, config)
    private val scope = CoroutineScope(SupervisorJob() + dispatcher)

    private val _state = MutableStateFlow<FullState?>(null)
    /** Latest full state, `null` until the first fix is accepted. */
    val state: StateFlow<FullState?> = _state.asStateFlow()

    private val _position = MutableStateFlow<PositionState?>(null)
    /** Per-fix state; emits on every accepted fix. */
    val position: StateFlow<PositionState?> = _position.asStateFlow()

    private val _trip = MutableStateFlow<TripState?>(null)
    /** Slow-changing state; emits only when the core reports a change. */
    val trip: StateFlow<TripState?> = _trip.asStateFlow()

    private val _errors = MutableSharedFlow<NavException>(extraBufferCapacity = 16)
    /** Fixes the core rejected. */
    val errors: SharedFlow<NavException> = _errors.asSharedFlow()

    private val _rerouteEvents = MutableSharedFlow<RerouteRequest>(extraBufferCapacity = 16)
    /** Every reroute request the core issued, including the final give-up notice. */
    val rerouteEvents: SharedFlow<RerouteRequest> = _rerouteEvents.asSharedFlow()

    private val _rerouteRequests = MutableStateFlow(0)
    /** Number of actionable (`OFF_ROUTE`) reroute requests issued so far. */
    val rerouteRequests: StateFlow<Int> = _rerouteRequests.asStateFlow()

    private val _routesInstalled = MutableStateFlow(0)
    /** Number of routes installed through [setRoute] (by the host or the replanner). */
    val routesInstalled: StateFlow<Int> = _routesInstalled.asStateFlow()

    private val _positionUpdates = MutableStateFlow(0)
    /** Accepted fixes so far (one [position] emission each). */
    val positionUpdates: StateFlow<Int> = _positionUpdates.asStateFlow()

    private val _tripUpdates = MutableStateFlow(0)
    /** [trip] emissions so far: only the updates the core flagged as changed. */
    val tripUpdates: StateFlow<Int> = _tripUpdates.asStateFlow()

    private val _callNanos = MutableSharedFlow<Long>(extraBufferCapacity = 1024)
    /** Wall time of each `updateLocation` boundary crossing, nanoseconds (for the demo's metrics). */
    val callNanos: SharedFlow<Long> = _callNanos.asSharedFlow()

    /** Fire-and-forget update from a platform [Location]. */
    fun update(location: Location) = update(location.toRawLocation())

    /** Fire-and-forget update; the result arrives on [position] / [trip]. */
    fun update(raw: RawLocation) {
        scope.launch { runUpdate(raw) }
    }

    /** Update and return the resulting state, or throw [NavException]. */
    suspend fun updateAndAwait(raw: RawLocation): FullState = withContext(dispatcher) {
        val next = timed { navigator.updateLocation(raw) }
        publish(next)
        next
    }

    /** Install a new route after rerouting. Progress restarts on the new route. */
    suspend fun setRoute(route: Route) = withContext(dispatcher) {
        navigator.setRoute(route)
        _state.value = null
        _position.value = null
        _trip.value = null
        _routesInstalled.value += 1
    }

    private fun runUpdate(raw: RawLocation) {
        try {
            publish(timed { navigator.updateLocation(raw) })
        } catch (e: NavException) {
            _errors.tryEmit(e)
        }
    }

    private inline fun <T> timed(block: () -> T): T {
        val start = System.nanoTime()
        try {
            return block()
        } finally {
            _callNanos.tryEmit(System.nanoTime() - start)
        }
    }

    private fun publish(next: FullState) {
        _position.value = next.position
        _positionUpdates.value += 1
        if (next.tripChanged) {
            _trip.value = next.trip
            _tripUpdates.value += 1
        }
        _state.value = next
        next.trip.reroute?.let { onReroute(it) }
    }

    private fun onReroute(request: RerouteRequest) {
        _rerouteEvents.tryEmit(request)
        if (request.reason != RerouteReason.OFF_ROUTE) return
        _rerouteRequests.value += 1
        val replan = replanner ?: return
        scope.launch {
            replan(request)?.let { setRoute(it) }
        }
    }

    override fun close() {
        scope.cancel()
        navigator.destroy()
    }
}

/** Convert a platform fix into the core's input type. Missing fields stay `null`. */
fun Location.toRawLocation(): RawLocation = RawLocation(
    point = GeoPoint(lat = latitude, lng = longitude),
    accuracyM = if (hasAccuracy()) accuracy.toDouble() else null,
    speedMps = if (hasSpeed()) speed.toDouble() else null,
    courseDeg = if (hasBearing()) bearing.toDouble() else null,
    timestampMs = time.toULong(),
)
