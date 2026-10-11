package com.navsdk.push

import android.location.Location
import com.navsdk.push.core.ErrorKind
import com.navsdk.push.core.GeoPoint
import com.navsdk.push.core.NavigationController
import com.navsdk.push.core.NavigationListener
import com.navsdk.push.core.NavigatorConfig
import com.navsdk.push.core.PositionState
import com.navsdk.push.core.RawLocation
import com.navsdk.push.core.ReplanningService
import com.navsdk.push.core.RerouteReason
import com.navsdk.push.core.RerouteRequest
import com.navsdk.push.core.Route
import com.navsdk.push.core.SessionState
import com.navsdk.push.core.TripState
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** A rejected fix or failed route install reported by the core. */
data class NavErrorEvent(val kind: ErrorKind, val message: String)

/**
 * Approach B, **push**: idiomatic wrapper over the BoltFFI [NavigationController].
 *
 * Fixes are handed to Rust on [dispatcher] (a single thread, never main) and
 * Rust calls back through a [NavigationListener] on that same thread: a
 * [PositionState] for every accepted fix, a [TripState] only when it changed,
 * and a [SessionState] on lifecycle events. The callbacks land in the flows
 * below, so nothing is polled and nothing is returned across the boundary.
 *
 * Rerouting is core-owned: when a [replanner] is supplied the controller
 * calls it from a Rust helper thread, installs the returned route itself and
 * reports the outcome through [session]. Without one, requests are only
 * visible in [trip].
 *
 * Call [close] when done to release the native object.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class NavigationSessionPush(
    route: Route,
    config: NavigatorConfig = NavigatorConfig(),
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default.limitedParallelism(1),
    replanner: (suspend (RerouteRequest) -> Route?)? = null,
) : AutoCloseable {
    private val controller = NavigationController(route, config)
    private val scope = CoroutineScope(SupervisorJob() + dispatcher)

    private val _position = MutableStateFlow<PositionState?>(null)
    /** Per-fix state; emits on every accepted fix. */
    val position: StateFlow<PositionState?> = _position.asStateFlow()

    private val _trip = MutableStateFlow<TripState?>(null)
    /** Slow-changing state; emits only when the core sends it. */
    val trip: StateFlow<TripState?> = _trip.asStateFlow()

    private val _session = MutableStateFlow(controller.session())
    /** Lifecycle: routes installed, replans in flight / succeeded / failed. */
    val session: StateFlow<SessionState> = _session.asStateFlow()

    private val _errors = MutableSharedFlow<NavErrorEvent>(extraBufferCapacity = 16)
    /** Fixes the core rejected. */
    val errors: SharedFlow<NavErrorEvent> = _errors.asSharedFlow()

    private val _rerouteEvents = MutableSharedFlow<RerouteRequest>(extraBufferCapacity = 16)
    /** Every reroute request the core issued, including the final give-up notice. */
    val rerouteEvents: SharedFlow<RerouteRequest> = _rerouteEvents.asSharedFlow()

    private val _rerouteRequests = MutableStateFlow(0)
    /** Number of actionable (`OFF_ROUTE`) reroute requests issued so far. */
    val rerouteRequests: StateFlow<Int> = _rerouteRequests.asStateFlow()

    private val _positionCallbacks = MutableStateFlow(0)
    /** How many `onPosition` callbacks crossed the boundary. */
    val positionCallbacks: StateFlow<Int> = _positionCallbacks.asStateFlow()

    private val _tripCallbacks = MutableStateFlow(0)
    /** How many `onTrip` callbacks crossed the boundary. */
    val tripCallbacks: StateFlow<Int> = _tripCallbacks.asStateFlow()

    private val _callNanos = MutableSharedFlow<Long>(extraBufferCapacity = 1024)
    /**
     * Wall time of each `updateLocation` boundary crossing, nanoseconds,
     * including the callbacks Rust makes before returning (for the demo's metrics).
     */
    val callNanos: SharedFlow<Long> = _callNanos.asSharedFlow()

    private val listener = object : NavigationListener {
        override fun onPosition(position: PositionState) {
            _position.value = position
            _positionCallbacks.update { it + 1 }
        }

        override fun onTrip(trip: TripState) {
            _trip.value = trip
            _tripCallbacks.update { it + 1 }
            trip.reroute?.let { request ->
                _rerouteEvents.tryEmit(request)
                if (request.reason == RerouteReason.OFF_ROUTE) _rerouteRequests.update { it + 1 }
            }
        }

        override fun onSession(session: SessionState) {
            _session.value = session
        }

        override fun onError(kind: ErrorKind, message: String) {
            _errors.tryEmit(NavErrorEvent(kind, message))
        }
    }

    private val listenerId = controller.addListener(listener)

    init {
        if (replanner != null) {
            controller.setReplanner(object : ReplanningService {
                override suspend fun replan(request: RerouteRequest): Route? = replanner(request)
            })
        }
    }

    /** Fire-and-forget update from a platform [Location]. */
    fun update(location: Location) = update(location.toRawLocation())

    /** Fire-and-forget update; results arrive on [position] / [trip]. */
    fun update(raw: RawLocation) {
        scope.launch { timed { controller.updateLocation(raw) } }
    }

    /** Hand several fixes to Rust in one boundary crossing, in order. */
    fun updateBatch(fixes: List<RawLocation>) {
        scope.launch { controller.updateLocationBatch(fixes) }
    }

    /**
     * Update and wait until the core has called back. Returns the position
     * after this fix, or `null` if the fix was rejected (see [errors]).
     */
    suspend fun updateAndAwait(raw: RawLocation): PositionState? = withContext(dispatcher) {
        val before = _positionCallbacks.value
        timed { controller.updateLocation(raw) }
        if (_positionCallbacks.value == before) null else _position.value
    }

    private inline fun timed(block: () -> Unit) {
        val start = System.nanoTime()
        try {
            block()
        } finally {
            _callNanos.tryEmit(System.nanoTime() - start)
        }
    }

    /** Install a new route by hand. Progress restarts on the new route. */
    suspend fun setRoute(route: Route) = withContext(dispatcher) {
        controller.setRoute(route)
        _position.value = null
        _trip.value = null
    }

    /** Stop accepting fixes until the next [setRoute]. */
    fun stop() {
        scope.launch { controller.stop() }
    }

    override fun close() {
        controller.removeListener(listenerId)
        scope.cancel()
        controller.close()
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
