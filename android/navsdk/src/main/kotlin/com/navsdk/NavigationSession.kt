package com.navsdk

import android.location.Location
import com.navsdk.core.GeoPoint
import com.navsdk.core.NavException
import com.navsdk.core.Navigator
import com.navsdk.core.NavigatorConfig
import com.navsdk.core.RawLocation
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
 * Idiomatic wrapper over the UniFFI [Navigator].
 *
 * All calls into Rust run on [dispatcher] (never the main thread); the latest
 * [TripState] is published through [state]. Rejected fixes (stale, implausible,
 * invalid) surface on [errors] and leave [state] untouched.
 *
 * Call [close] when done to release the native object.
 */
class NavigationSession(
    route: Route,
    config: NavigatorConfig = NavigatorConfig(),
    private val dispatcher: CoroutineDispatcher = Dispatchers.Default,
) : AutoCloseable {
    private val navigator = Navigator(route, config)
    private val scope = CoroutineScope(SupervisorJob() + dispatcher)

    private val _state = MutableStateFlow<TripState?>(null)
    /** Latest trip state, `null` until the first fix is accepted. */
    val state: StateFlow<TripState?> = _state.asStateFlow()

    private val _errors = MutableSharedFlow<NavException>(extraBufferCapacity = 16)
    /** Fixes the core rejected. */
    val errors: SharedFlow<NavException> = _errors.asSharedFlow()

    /** Number of reroute requests the core has issued so far. */
    private val _rerouteRequests = MutableStateFlow(0)
    val rerouteRequests: StateFlow<Int> = _rerouteRequests.asStateFlow()

    /** Fire-and-forget update from a platform [Location]. */
    fun update(location: Location) = update(location.toRawLocation())

    /** Fire-and-forget update; the result arrives on [state]. */
    fun update(raw: RawLocation) {
        scope.launch { runUpdate(raw) }
    }

    /** Update and return the resulting state, or throw [NavException]. */
    suspend fun updateAndAwait(raw: RawLocation): TripState = withContext(dispatcher) {
        val next = navigator.updateLocation(raw)
        publish(next)
        next
    }

    /** Install a new route after rerouting. Progress restarts on the new route. */
    suspend fun setRoute(route: Route) = withContext(dispatcher) {
        navigator.setRoute(route)
        _state.value = null
    }

    private fun runUpdate(raw: RawLocation) {
        try {
            publish(navigator.updateLocation(raw))
        } catch (e: NavException) {
            _errors.tryEmit(e)
        }
    }

    private fun publish(next: TripState) {
        if (next.needsReroute) _rerouteRequests.value += 1
        _state.value = next
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
