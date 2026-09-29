// ==UserScript==
// @version      7.0.5
// @description  SkyWalking 日志查询插件 v7.0.5：液态玻璃日志检索、调用链分析与进程内资源争抢证据诊断，支持深色主题
// @grant        none
// ==/UserScript==

(function () {
    'use strict';
    if (window.__SW_LOG_QUERY_V7__) return;
    window.__SW_LOG_QUERY_V7__ = true;

    const GRAPHQL_URL = location.origin + '/graphql';
    const SW_TS = 'sw-' + Math.random().toString(36).slice(2, 8);

    // ==================== Theme（深色主题：面板与详情弹窗挂 sw-dark，状态持久化） ====================
    const THEME_KEY = 'sw_theme_v1';
    let darkMode = (function () {
        try {
            var v = localStorage.getItem(THEME_KEY);
            if (v) return v === 'dark';
        } catch (e) {}
        return !!(window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches);
    })();
    function applyTheme() {
        var wrap = document.getElementById(SW_TS + '-wrap');
        var modal = document.getElementById(SW_TS + '-modal');
        if (wrap) wrap.classList.toggle('sw-dark', darkMode);
        if (modal) modal.classList.toggle('sw-dark', darkMode);
        var btn = document.getElementById(SW_TS + '-theme-btn');
        if (btn) btn.innerHTML = darkMode ? '&#9728; 浅色' : '&#127769; 深色';
    }
    function toggleTheme() {
        darkMode = !darkMode;
        try { localStorage.setItem(THEME_KEY, darkMode ? 'dark' : 'light'); } catch (e) {}
        applyTheme();
    }

    // ==================== State ====================
    let tabs = [];
    let activeTabIndex = -1;
    let serviceMap = {};
    let originalNodes = [];
    let ensureButtonInterval;
    let autoQueryTimer = null;
    // 切 tab 时短暂抑制 scroll 事件处理：避免旧 tab 的滚动位置带到新 tab 后误触发分页查询
    // 500ms 窗口覆盖主线程繁忙时浏览器延迟触发的 scroll 事件（且必须先于 DOM 变更设置）
    let suppressScrollUntil = 0;
    let saveTimer = null;
    let diagnosisController = null;
    let diagnosisRunning = false;
    let diagnosisCapabilitiesCache = null;
    let diagnosisCapabilitiesTs = 0;
    let diagnosisGeneration = 0;
    let servicesReady = false;
    let servicesReadyPromise = null;
    const SERVICES_CACHE_KEY = 'sw_services_cache_v1';
    let servicesCacheTs = 0;

    // ==================== Utilities ====================
    function pad(n) { return n.toString().padStart(2, '0'); }
    function getLocalDatetime(offsetMinutes) {
        offsetMinutes = offsetMinutes || 0;
        var now = new Date();
        now.setMinutes(now.getMinutes() + offsetMinutes);
        var localISO = new Date(now.getTime() - now.getTimezoneOffset() * 60000).toISOString();
        // 秒级精度：YYYY-MM-DDTHH:mm:ss（datetime-local step="1" 配套）
        return localISO.slice(0, 19);
    }
    function formatToSkywalkingTime(localStr, step) {
        if (!localStr) return '';
        step = step || 'MINUTE';
        var d = new Date(localStr);
        var year = d.getFullYear();
        var month = pad(d.getMonth() + 1);
        var day = pad(d.getDate());
        var hour = pad(d.getHours());
        if (step === 'HOUR') return year + '-' + month + '-' + day + ' ' + hour;
        var minute = pad(d.getMinutes());
        if (step === 'SECOND') {
            var second = pad(d.getSeconds());
            return year + '-' + month + '-' + day + ' ' + hour + minute + second;
        }
        return year + '-' + month + '-' + day + ' ' + hour + minute;
    }
    function buildDiagnosisDuration(startInput, endInput) {
        var start = new Date(startInput);
        var end = new Date(endInput);
        if (!isFinite(start.getTime()) || !isFinite(end.getTime()) || end <= start) {
            throw new Error('诊断时间范围无效');
        }
        var span = end.getTime() - start.getTime();
        var step = span >= 24 * 60 * 60 * 1000 ? 'HOUR' : 'MINUTE';
        var baselineStart = new Date(start.getTime() - span);
        var alignedIncidentStart = new Date(start.getTime());
        alignedIncidentStart.setSeconds(0, 0);
        if (step === 'HOUR') alignedIncidentStart.setMinutes(0, 0, 0);
        return {
            step: step,
            incident: {
                start: formatToSkywalkingTime(startInput, step),
                end: formatToSkywalkingTime(endInput, step),
                step: step
            },
            combined: {
                start: formatToSkywalkingTime(new Date(baselineStart.getTime() - baselineStart.getTimezoneOffset() * 60000).toISOString().slice(0, 19), step),
                end: formatToSkywalkingTime(endInput, step),
                step: step
            },
            incidentStartKey: diagnosisTimeKey(alignedIncidentStart)
        };
    }
    function diagnosisTimeKey(value) {
        if (value instanceof Date) {
            return Number(
                value.getFullYear().toString() +
                pad(value.getMonth() + 1) +
                pad(value.getDate()) +
                pad(value.getHours()) +
                pad(value.getMinutes())
            );
        }
        var raw = String(value == null ? '' : value).replace(/\D/g, '');
        if (raw.length === 13 && Number(raw) > 1000000000000) return diagnosisTimeKey(new Date(Number(raw)));
        if (raw.length >= 12) return Number(raw.slice(0, 12));
        if (raw.length === 10) return Number(raw + '00');
        return Number(raw) || 0;
    }
    function normalizeMetricPoints(values) {
        return (values || []).map(function (point) {
            return {
                id: String(point && point.id != null ? point.id : ''),
                key: diagnosisTimeKey(point && point.id),
                value: Number(point && point.value)
            };
        }).filter(function (point) {
            return point.key > 0 && isFinite(point.value);
        }).sort(function (a, b) { return a.key - b.key; });
    }
    function median(values) {
        var sorted = (values || []).filter(isFinite).slice().sort(function (a, b) { return a - b; });
        if (!sorted.length) return 0;
        var mid = Math.floor(sorted.length / 2);
        return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
    }
    function medianAbsoluteDeviation(values, center) {
        if (!values || !values.length) return 0;
        center = center == null ? median(values) : center;
        return median(values.map(function (value) { return Math.abs(value - center); }));
    }
    function labelsArrayToObject(labels) {
        var out = {};
        if (Array.isArray(labels)) {
            labels.forEach(function (item) {
                if (item && item.key != null) out[String(item.key)] = String(item.value == null ? '' : item.value);
            });
        } else if (labels && typeof labels === 'object') {
            Object.keys(labels).forEach(function (key) { out[key] = String(labels[key]); });
        }
        return out;
    }
    function normalizeMqeSeries(expressionResult) {
        if (!expressionResult || expressionResult.type === 'UNKNOWN') return [];
        var out = [];
        (expressionResult.results || []).forEach(function (result) {
            out.push({
                labels: labelsArrayToObject(result && result.metric && result.metric.labels),
                values: normalizeMetricPoints(result && result.values)
            });
        });
        return out;
    }
    function resourceRole(labelValue) {
        var value = String(labelValue || '').toLowerCase().replace(/[\s.-]+/g, '_');
        if (/(^|_)(active|active_count|active_size|in_use|using)($|_)/.test(value)) return 'active';
        if (/(^|_)(max|maximum|max_count|max_size|maximum_pool_size)($|_)/.test(value)) return 'max';
        return '';
    }
    function diagnosisKeyToEpochMinutes(key) {
        var raw = String(key || '').padStart(12, '0');
        if (raw.length < 12) return 0;
        var date = new Date(
            Number(raw.slice(0, 4)),
            Number(raw.slice(4, 6)) - 1,
            Number(raw.slice(6, 8)),
            Number(raw.slice(8, 10)),
            Number(raw.slice(10, 12))
        );
        return Math.floor(date.getTime() / 60000);
    }
    function longestSaturatedRun(points, step) {
        var longest = 0, current = 0;
        var previousMinute = null;
        var expectedGap = step === 'HOUR' ? 60 : 1;
        (points || []).forEach(function (point) {
            var minute = diagnosisKeyToEpochMinutes(point.key);
            if (previousMinute != null && minute - previousMinute !== expectedGap) current = 0;
            if (point.ratio >= 0.85) {
                current++;
                if (current > longest) longest = current;
            } else {
                current = 0;
            }
            previousMinute = minute;
        });
        return longest;
    }
    function pairResourceSeries(series, instanceName, resourceType, incidentStartKey, step) {
        var groups = {};
        (series || []).forEach(function (item) {
            var labels = labelsArrayToObject(item.labels);
            var typeLabel = labels.metric_type || labels.status || labels.type || '';
            var role = resourceRole(typeLabel);
            if (!role) return;
            var poolName = labels.pool_name || labels.name || labels.datasource || labels.pool || 'default';
            var key = String(poolName);
            if (!groups[key]) groups[key] = { active: [], max: [], labels: labels };
            groups[key][role] = normalizeMetricPoints(item.values);
        });
        return Object.keys(groups).map(function (poolName) {
            var group = groups[poolName];
            var maxCursor = 0;
            var lastMaxPoint = null;
            var maxAgeMinutes = (step === 'HOUR' ? 60 : 1) * 2;
            var paired = group.active.map(function (point) {
                while (maxCursor < group.max.length && group.max[maxCursor].key <= point.key) {
                    lastMaxPoint = group.max[maxCursor++];
                }
                var maxAge = lastMaxPoint
                    ? diagnosisKeyToEpochMinutes(point.key) - diagnosisKeyToEpochMinutes(lastMaxPoint.key)
                    : Infinity;
                var max = lastMaxPoint && maxAge >= 0 && maxAge <= maxAgeMinutes ? lastMaxPoint.value : 0;
                return {
                    id: point.id,
                    key: point.key,
                    active: point.value,
                    max: max,
                    ratio: max > 0 ? point.value / max : null
                };
            });
            var incidentPoints = incidentStartKey
                ? paired.filter(function (point) { return point.key >= incidentStartKey; })
                : paired;
            var ratios = incidentPoints.filter(function (point) { return point.ratio != null; });
            var peakPoint = ratios.reduce(function (best, point) {
                return !best || point.ratio > best.ratio ? point : best;
            }, null);
            var saturated = ratios.filter(function (point) { return point.ratio >= 0.85; });
            return {
                instanceName: instanceName || '未知实例',
                resourceType: resourceType || 'resource',
                poolName: poolName,
                capacityKnown: !!ratios.length,
                peakRatio: peakPoint ? peakPoint.ratio : null,
                peakActive: peakPoint ? peakPoint.active : (incidentPoints.length ? Math.max.apply(null, incidentPoints.map(function (p) { return p.active; })) : 0),
                peakMax: peakPoint ? peakPoint.max : null,
                maxConsecutiveSaturated: longestSaturatedRun(incidentPoints, step || 'MINUTE'),
                onsetKey: saturated.length ? saturated[0].key : null,
                saturatedKeys: saturated.map(function (point) { return point.key; }),
                points: paired
            };
        });
    }
    function classifyEndpoint(name) {
        name = String(name || '');
        if (/(^|[\s:/_-])(mq|consumer|listener|topic)([\s:/_-]|$)/i.test(name)) return 'MQ';
        if (/(^|[\s:/_-])(download|export|file)([\s:/_-]|$)/i.test(name)) return '任务/下载';
        if (/^\//.test(name) || /api|controller|http/i.test(name)) return 'API';
        return '未知';
    }
    function buildCandidateProfile(candidate, incidentStartKey) {
        var latency = normalizeMetricPoints(candidate.latency);
        var cpm = normalizeMetricPoints(candidate.cpm);
        var baseline = latency.filter(function (point) { return point.key < incidentStartKey && point.value > 0; });
        var incident = latency.filter(function (point) { return point.key >= incidentStartKey && point.value > 0; });
        var baselineValues = baseline.map(function (point) { return point.value; });
        var baselineMedian = median(baselineValues);
        var baselineMad = medianAbsoluteDeviation(baselineValues, baselineMedian);
        var threshold = baselineMedian > 0
            ? Math.max(baselineMedian * 1.5, baselineMedian + Math.max(100, baselineMad * 3))
            : 10000;
        var anomalous = incident.filter(function (point) { return point.value >= threshold || point.value >= 10000; });
        var cpmByKey = {};
        cpm.forEach(function (point) { cpmByKey[point.key] = point.value; });
        var concurrency = incident.map(function (point) {
            return {
                key: point.key,
                value: Math.max(0, cpmByKey[point.key] || 0) * Math.max(0, point.value) / 60000
            };
        });
        var peakLatency = incident.length ? Math.max.apply(null, incident.map(function (point) { return point.value; })) : 0;
        var currentLatency = incident.length ? incident[incident.length - 1].value : 0;
        return {
            name: candidate.name || '未知端点',
            id: candidate.id || '',
            type: classifyEndpoint(candidate.name),
            baselineMedian: baselineMedian,
            baselineAvailable: baseline.length > 0,
            anomalyRatio: baselineMedian > 0 ? peakLatency / baselineMedian : (peakLatency > 0 ? Infinity : 0),
            threshold: threshold,
            currentLatency: currentLatency,
            peakLatency: peakLatency,
            peakConcurrency: concurrency.length ? Math.max.apply(null, concurrency.map(function (point) { return point.value; })) : 0,
            concurrencyPoints: concurrency,
            onsetKey: anomalous.length ? anomalous[0].key : null,
            anomalousKeys: anomalous.map(function (point) { return point.key; }),
            raw: candidate
        };
    }
    function diagnoseContention(input) {
        input = input || {};
        var resources = input.resources || [];
        var saturatedResources = resources.filter(function (resource) {
            return resource.capacityKnown && resource.peakRatio >= 0.85 && resource.maxConsecutiveSaturated >= 2;
        });
        var contentionConfirmed = saturatedResources.length > 0;
        var resourceOnset = saturatedResources.reduce(function (min, resource) {
            return resource.onsetKey && (!min || resource.onsetKey < min) ? resource.onsetKey : min;
        }, null);
        var saturatedKeySet = {};
        saturatedResources.forEach(function (resource) {
            (resource.saturatedKeys || []).forEach(function (key) { saturatedKeySet[key] = true; });
        });
        var profiles = (input.candidates || []).map(function (candidate) {
            return buildCandidateProfile(candidate, input.incidentStartKey || 0);
        });
        var concurrencyTotalsByKey = {};
        profiles.forEach(function (profile) {
            profile.concurrencyPoints.forEach(function (point) {
                if (!saturatedKeySet[point.key]) return;
                concurrencyTotalsByKey[point.key] = (concurrencyTotalsByKey[point.key] || 0) + point.value;
            });
        });
        profiles.forEach(function (profile) {
            var shares = profile.concurrencyPoints.map(function (point) {
                var total = concurrencyTotalsByKey[point.key] || 0;
                return saturatedKeySet[point.key] && total > 0 ? point.value / total : null;
            }).filter(function (share) { return share != null; });
            profile.concurrencyShare = shares.length ? median(shares) : 0;
            var overlap = profile.anomalousKeys.filter(function (key) { return saturatedKeySet[key]; }).length;
            var score = 0;
            score += Math.min(30, profile.concurrencyShare * 35);
            if (resourceOnset && profile.onsetKey && profile.onsetKey <= resourceOnset) score += 20;
            score += Math.min(20, overlap * 8);
            if (isFinite(profile.anomalyRatio)) score += Math.min(20, Math.max(0, profile.anomalyRatio - 1) * 6);
            else if (profile.peakLatency > 0) score += 12;
            if (profile.type === 'MQ' || profile.type === '任务/下载') score += 5;
            if (profile.peakLatency >= 10000) score += 5;
            if (!contentionConfirmed) score = Math.min(score, 39);
            if (!profile.baselineAvailable) score = Math.min(score, 69);
            if ((input.instanceCount || 0) > 1) score = Math.min(score, 69);
            profile.score = Math.round(score);
            profile.confidence = score >= 75 ? 'high' : (score >= 50 ? 'medium' : 'low');
            profile.evidence = [];
            if (profile.concurrencyShare >= 0.35) profile.evidence.push('饱和同桶内候选集合估算占用占比高');
            if (resourceOnset && profile.onsetKey && profile.onsetKey <= resourceOnset) profile.evidence.push('异常早于或同步于资源饱和');
            if (overlap) profile.evidence.push('与资源饱和重叠 ' + overlap + ' 个时间桶');
            if (!profile.baselineAvailable) profile.evidence.push('缺少历史基线');
            if ((input.instanceCount || 0) > 1) profile.evidence.push('端点指标为服务聚合，无法直接绑定单个 JVM');
        });
        var victims = profiles.filter(function (profile) {
            return contentionConfirmed &&
                profile.type === 'API' &&
                profile.onsetKey &&
                resourceOnset &&
                profile.onsetKey >= resourceOnset &&
                profile.concurrencyShare < 0.25 &&
                profile.anomalyRatio >= 1.5;
        }).sort(function (a, b) { return b.anomalyRatio - a.anomalyRatio; }).slice(0, 3);
        var victimNames = {};
        victims.forEach(function (victim) { victimNames[victim.name] = true; });
        var suspects = profiles.filter(function (profile) {
            return !victimNames[profile.name] && (profile.onsetKey || profile.peakLatency >= 10000);
        }).sort(function (a, b) { return b.score - a.score; });
        return {
            contentionConfirmed: contentionConfirmed,
            resourceOnset: resourceOnset,
            suspects: suspects,
            victims: victims,
            resources: resources,
            saturatedResources: saturatedResources,
            missingCapacity: resources.filter(function (resource) { return !resource.capacityKnown; })
        };
    }
    function extractNativeDiagnosisContext(href, services, defaultStart, defaultEnd) {
        href = String(href || '');
        services = services || {};
        var queryText = '';
        var questionIndex = href.indexOf('?');
        if (questionIndex >= 0) queryText = href.slice(questionIndex + 1).split('#')[0];
        var hashQuestion = href.indexOf('#');
        if (hashQuestion >= 0) {
            var hash = href.slice(hashQuestion + 1);
            var hashQueryIndex = hash.indexOf('?');
            if (hashQueryIndex >= 0) queryText += (queryText ? '&' : '') + hash.slice(hashQueryIndex + 1);
        }
        var params = new URLSearchParams(queryText);
        var serviceName = params.get('serviceName') || params.get('service') || params.get('serviceCode') || '';
        var serviceId = params.get('serviceId') || '';
        if (!serviceName && serviceId) {
            Object.keys(services).some(function (name) {
                if (String(services[name]) === String(serviceId)) {
                    serviceName = name;
                    return true;
                }
                return false;
            });
        }
        if (serviceName && !serviceId) serviceId = services[serviceName] || '';
        return {
            serviceName: serviceName,
            serviceId: serviceId,
            startTime: params.get('startTime') || params.get('start') || defaultStart || getLocalDatetime(-30),
            endTime: params.get('endTime') || params.get('end') || defaultEnd || getLocalDatetime(0),
            source: serviceName ? 'native-url' : 'unresolved'
        };
    }
    function getDiagnosisContext() {
        var tab = tabs[activeTabIndex];
        var pluginOpen = !!document.getElementById(SW_TS + '-wrap');
        if (pluginOpen && tab && String(tab.serviceInput || '').trim()) {
            var tabService = String(tab.serviceInput || '').trim();
            return {
                serviceName: tabService,
                serviceId: serviceMap[tabService] || '',
                startTime: tab.startTime || getLocalDatetime(-30),
                endTime: tab.endTime || getLocalDatetime(0),
                source: 'active-tab'
            };
        }
        return extractNativeDiagnosisContext(location.href, serviceMap, getLocalDatetime(-30), getLocalDatetime(0));
    }
    function esc(s) {
        return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) {
            return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
        });
    }
    function renderServiceDatalistOptions(services) {
        services = services || {};
        return Object.keys(services).sort().map(function (name) {
            return '<option value="' + esc(name) + '"></option>';
        }).join('');
    }
    function renderDiagnosisAlgorithmExplanation() {
        return '<div class="' + SW_TS + '-diag-card">' +
            '<h3>诊断算法说明</h3>' +
            '<div class="' + SW_TS + '-diag-meta" style="font-size:12px;line-height:1.8">' +
                '<b style="color:#fff">1. 候选端点：</b>合并当前服务的延迟 Top 15 与 CPM Top 15。<br>' +
                '<b style="color:#fff">2. 异常基线：</b>使用所选时间段之前的等长窗口，按中位数与 MAD 判断异常；10 秒仅作兜底信号。<br>' +
                '<b style="color:#fff">3. 资源争抢：</b>线程池或连接池 active/max 连续至少 2 个时间桶达到 85%，才确认资源饱和。<br>' +
                '<b style="color:#fff">4. 嫌疑评分：</b>综合同一饱和时间桶内的估算占用、异常先后、重叠时间与异常幅度。估算占用 = CPM × 平均延迟 ÷ 60000。<br>' +
                '<b style="color:#fff">5. 受影响 API：</b>资源饱和后才变慢、且候选集合内估算占比较低的常规 API。' +
            '</div>' +
            '<div class="' + SW_TS + '-diag-warn"><b>准确性边界：</b>结果是相关性嫌疑排序，不等于真实根因。端点指标是服务级聚合，多实例、TopN 之外端点、CPU/GC/锁竞争和下游故障都可能造成偏差。</div>' +
        '</div>';
    }
    function highlight(text, kws) {
        var safe = esc(text);
        if (!kws || !kws.length) return safe;
        try {
            for (var i = 0; i < kws.length; i++) {
                var kw = kws[i];
                if (!kw) continue;
                var re = new RegExp(kw.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'gi');
                safe = safe.replace(re, function (m) { return '<mark>' + m + '</mark>'; });
            }
            return safe;
        } catch (e) { return safe; }
    }
    function $(s, root) { return (root || document).querySelector(s); }
    function $$(s, root) { return Array.prototype.slice.call((root || document).querySelectorAll(s)); }
    function debounceSave() {
        clearTimeout(saveTimer);
        saveTimer = setTimeout(saveTabsState, 300);
    }

    // ==================== CSS ====================
    function injectStyle() {
        if (document.getElementById(SW_TS + '-style')) return;
        var css = [
            '#' + SW_TS + '-wrap{position:fixed;inset:0;z-index:99998;display:flex;flex-direction:column;background:radial-gradient(1200px 560px at 10% -12%,rgba(255,59,48,0.05) 0%,transparent 60%),radial-gradient(1000px 520px at 92% -8%,rgba(0,122,255,0.045) 0%,transparent 58%),linear-gradient(180deg,#fbfbfd 0%,#f3f4f7 55%,#eaeaef 100%);font-family:-apple-system,BlinkMacSystemFont,"SF Pro Text","SF Pro Display","Helvetica Neue",Helvetica,Arial,sans-serif;color:#1d1d1f;-webkit-font-smoothing:antialiased}',
            '#' + SW_TS + '-wrap *{box-sizing:border-box}',

            // Header
            '.' + SW_TS + '-header{background:linear-gradient(180deg,rgba(255,255,255,0.86) 0%,rgba(255,255,255,0.68) 100%);backdrop-filter:blur(40px) saturate(180%);-webkit-backdrop-filter:blur(40px) saturate(180%);border-bottom:1px solid rgba(0,0,0,0.055);padding:8px 16px;display:flex;align-items:center;justify-content:space-between;box-shadow:0 1px 0 rgba(255,255,255,0.7),0 8px 24px rgba(16,24,40,0.035);flex-shrink:0}',
            '.' + SW_TS + '-hl{display:flex;align-items:center;gap:12px}',
            '.' + SW_TS + '-logo{width:28px;height:28px;background:linear-gradient(135deg,#ff4b3d,#ff8a3d);border-radius:8px;display:flex;align-items:center;justify-content:center;color:#fff;font-size:15px;font-weight:700;box-shadow:0 1px 2px rgba(255,59,48,0.3),0 4px 12px rgba(255,59,48,0.22),inset 0 1px 0 rgba(255,255,255,0.35)}',
            '.' + SW_TS + '-title{font-size:15px;font-weight:600;letter-spacing:-0.2px}',
            '.' + SW_TS + '-title span{font-size:11px;font-weight:400;color:#86868b;margin-left:6px}',
            '.' + SW_TS + '-theme-btn{padding:5px 10px;border:1px solid rgba(0,0,0,0.075);background:linear-gradient(180deg,rgba(255,255,255,0.92),rgba(255,255,255,0.74));backdrop-filter:blur(10px);-webkit-backdrop-filter:blur(10px);border-radius:7px;font-size:12px;color:#3a3a3c;cursor:pointer;transition:background 0.2s,border-color 0.2s,color 0.2s,box-shadow 0.2s;display:flex;align-items:center;gap:4px;box-shadow:0 1px 2px rgba(16,24,40,0.04),inset 0 1px 0 #fff}',
            '.' + SW_TS + '-theme-btn:hover{background:#fff;border-color:rgba(0,0,0,0.12);box-shadow:0 2px 6px rgba(16,24,40,0.06),inset 0 1px 0 #fff}',
            '.' + SW_TS + '-hr{display:flex;align-items:center;gap:8px}',
            '.' + SW_TS + '-exit{padding:5px 10px;border:1px solid rgba(0,0,0,0.075);background:linear-gradient(180deg,rgba(255,255,255,0.92),rgba(255,255,255,0.74));backdrop-filter:blur(10px);-webkit-backdrop-filter:blur(10px);border-radius:7px;font-size:12px;color:#86868b;cursor:pointer;transition:background 0.2s,border-color 0.2s,color 0.2s,box-shadow 0.2s;display:flex;align-items:center;gap:4px;box-shadow:0 1px 2px rgba(16,24,40,0.04),inset 0 1px 0 #fff}',
            '.' + SW_TS + '-exit:hover{background:#fff;color:#1d1d1f;border-color:rgba(0,0,0,0.12)}',

            // Body
            '#' + SW_TS + '-body{flex:1;overflow:hidden;padding:10px 16px 14px;display:flex;flex-direction:column;gap:8px;min-height:0}',

            // Sub-tab nav
            '.' + SW_TS + '-subnav{display:flex;align-items:center;gap:5px;padding:2px 0 4px;overflow-x:auto;flex-shrink:0}',
            '.' + SW_TS + '-sub-btn{padding:5px 10px;border:1px solid rgba(0,0,0,0.06);background:linear-gradient(180deg,rgba(255,255,255,0.82) 0%,rgba(255,255,255,0.6) 100%);backdrop-filter:blur(15px) saturate(150%);-webkit-backdrop-filter:blur(15px) saturate(150%);border-radius:7px;font-size:12px;color:#3a3a3c;cursor:pointer;transition:background 0.2s,border-color 0.2s,color 0.2s,box-shadow 0.2s;display:flex;align-items:center;gap:6px;white-space:nowrap;flex-shrink:0;box-shadow:0 1px 2px rgba(16,24,40,0.035),inset 0 1px 0 rgba(255,255,255,0.9)}',
            '.' + SW_TS + '-sub-btn:hover{background:linear-gradient(180deg,#fff 0%,rgba(255,255,255,0.86) 100%);color:#1d1d1f;border-color:rgba(0,0,0,0.1)}',
            '.' + SW_TS + '-sub-btn.active{background:linear-gradient(180deg,#fff 0%,rgba(255,255,255,0.9) 100%);border-color:rgba(255,59,48,0.32);color:#1d1d1f;box-shadow:0 1px 2px rgba(16,24,40,0.05),0 6px 16px rgba(255,59,48,0.09),inset 0 1px 0 #fff;font-weight:500}',
            '.' + SW_TS + '-sub-btn.' + SW_TS + '-dragging{opacity:0.4}',
            '.' + SW_TS + '-sub-btn.' + SW_TS + '-drop-l{box-shadow:inset 3px 0 0 #ff3b30}',
            '.' + SW_TS + '-sub-btn.' + SW_TS + '-drop-r{box-shadow:inset -3px 0 0 #ff3b30}',
            '.' + SW_TS + '-sub-btn .tab-label{user-select:none;cursor:pointer;max-width:160px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;display:inline-block}',
            '.' + SW_TS + '-sub-btn .tab-label:hover{background:rgba(255,59,48,0.07);border-radius:3px}',
            '.' + SW_TS + '-sub-btn .tab-label.editing{user-select:text;overflow:visible;background:#fff;border-radius:3px;box-shadow:0 0 0 1.5px #ff3b30,0 0 0 3px rgba(255,59,48,0.14);padding:1px 2px;cursor:text}',
            '#' + SW_TS + '-wrap .tab-label-input{font-size:12px;padding:0 2px;border:none;background:transparent;outline:none;width:140px;color:#1d1d1f;height:18px;margin:0;font-family:inherit}',
            '.' + SW_TS + '-sub-btn .cs{font-size:13px;color:#aeaeb5;cursor:pointer;padding:0 2px;border-radius:4px;transition:background 0.15s,color 0.15s}',
            '.' + SW_TS + '-sub-btn .cs:hover{background:rgba(255,59,48,0.1);color:#ff3b30}',
            '.' + SW_TS + '-add-btn{padding:5px 10px;border:1px dashed rgba(0,0,0,0.14);background:rgba(255,255,255,0.42);border-radius:7px;font-size:12px;color:#86868b;cursor:pointer;transition:background 0.2s,border-color 0.2s,color 0.2s;flex-shrink:0}',
            '.' + SW_TS + '-add-btn:hover{background:rgba(255,255,255,0.8);color:#1d1d1f;border-color:rgba(255,59,48,0.4)}',
            '.' + SW_TS + '-close-all-btn{padding:5px 10px;border:1px solid rgba(255,59,48,0.22);background:rgba(255,59,48,0.06);border-radius:7px;font-size:11px;color:#d70015;cursor:pointer;transition:background 0.2s,border-color 0.2s;flex-shrink:0}',
            '.' + SW_TS + '-close-all-btn:hover{background:rgba(255,59,48,0.12);border-color:rgba(255,59,48,0.34)}',

            // Form (compact, no folding)
            '.' + SW_TS + '-qcard{background:linear-gradient(180deg,rgba(255,255,255,0.88) 0%,rgba(255,255,255,0.72) 100%);backdrop-filter:blur(40px) saturate(180%);-webkit-backdrop-filter:blur(40px) saturate(180%);border:1px solid rgba(0,0,0,0.055);border-radius:14px;padding:12px 16px;box-shadow:0 1px 2px rgba(16,24,40,0.035),0 14px 34px rgba(16,24,40,0.07),inset 0 1px 0 rgba(255,255,255,0.95);flex-shrink:0;container-type:inline-size;container-name:qform}',
            '.' + SW_TS + '-frow{display:flex;flex-wrap:wrap;gap:10px 14px;align-items:flex-end}',
            '.' + SW_TS + '-qgrid{display:grid;gap:10px 14px;align-items:end;justify-content:space-between;grid-template-columns:minmax(220px,320px) minmax(280px,420px) minmax(220px,300px) auto}',
            '.' + SW_TS + '-qtrange{display:flex;flex-wrap:wrap;gap:12px;grid-column:2/4;min-width:0}',
            '.' + SW_TS + '-qtrange .' + SW_TS + '-fg{flex:1 1 225px;min-width:225px;max-width:250px}',
            '@container (min-width:1780px){.' + SW_TS + '-qgrid{grid-template-columns:minmax(160px,300px) minmax(320px,620px) minmax(180px,240px) minmax(120px,140px) minmax(180px,240px) minmax(489px,500px) auto}.' + SW_TS + '-qtrange{grid-column:auto}}',
            '@container (max-width:1100px){.' + SW_TS + '-qgrid{grid-template-columns:minmax(330px,1.45fr) minmax(230px,1fr)}.' + SW_TS + '-q-trace{grid-column:1/-1}.' + SW_TS + '-qtrange,.' + SW_TS + '-qactions{grid-column:auto}}',
            '@container (max-width:620px){.' + SW_TS + '-qgrid{grid-template-columns:minmax(0,1fr)}.' + SW_TS + '-trange-arrow{display:none}}',
            '.' + SW_TS + '-fg{display:flex;flex-direction:column;gap:3px;min-width:120px;flex:1 1 120px}',
            '.' + SW_TS + '-fg label{font-size:10.5px;font-weight:600;color:#76767c;text-transform:uppercase;letter-spacing:0.55px}',
            '.' + SW_TS + '-fg input,.' + SW_TS + '-fg select{box-sizing:border-box;padding:6px 9px;border:1px solid rgba(0,0,0,0.085);border-radius:8px;font-size:12.5px;background:linear-gradient(180deg,#fff 0%,#fcfcfd 100%);color:#1d1d1f;transition:border-color 0.2s,box-shadow 0.2s,background 0.2s;outline:none;font-family:inherit;width:100%;height:32px;box-shadow:0 1px 1px rgba(16,24,40,0.025),inset 0 1px 1px rgba(16,24,40,0.02)}',
            '.' + SW_TS + '-fg input:focus,.' + SW_TS + '-fg select:focus{border-color:rgba(255,59,48,0.45);box-shadow:0 1px 1px rgba(16,24,40,0.02),0 0 0 3px rgba(255,59,48,0.12);background:#fff}',
            '.' + SW_TS + '-fg input::placeholder{color:#b4b5bb}',
            '.' + SW_TS + '-fg select{-webkit-appearance:none;appearance:none;background-image:url("data:image/svg+xml,%3csvg xmlns=\'http://www.w3.org/2000/svg\' fill=\'none\' viewBox=\'0 0 20 20\'%3e%3cpath stroke=\'%2386868b\' stroke-linecap=\'round\' stroke-linejoin=\'round\' stroke-width=\'1.5\' d=\'m6 8 4 4 4-4\'/%3e%3c/svg%3e");background-repeat:no-repeat;background-position:right 8px center;background-size:12px;padding-right:26px;cursor:pointer}',
            // 日期选择器图标：原生图标在浅底上对比不足，换成自带 SVG（深色块再换浅色版）
            '.' + SW_TS + '-fg input[type="datetime-local"]::-webkit-calendar-picker-indicator{opacity:1;cursor:pointer;border-radius:5px;background:url("data:image/svg+xml,%3csvg xmlns=\'http://www.w3.org/2000/svg\' fill=\'none\' viewBox=\'0 0 24 24\'%3e%3cpath stroke=\'%2348484c\' stroke-width=\'1.8\' stroke-linecap=\'round\' stroke-linejoin=\'round\' d=\'M4 7a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V7zM9 3v5M15 3v5M4 12h16\'/%3e%3c/svg%3e") center no-repeat;background-size:14px 14px;transition:background-color 0.15s}',
            '.' + SW_TS + '-fg input[type="datetime-local"]::-webkit-calendar-picker-indicator:hover{background-color:rgba(0,122,255,0.1)}',
            '.' + SW_TS + '-svc-ibtn{flex:0 0 30px;border:none;background:transparent;color:#86868b;border-radius:7px;font-size:14px;cursor:pointer;display:flex;align-items:center;justify-content:center;transition:color 0.15s,background 0.15s;padding:0}',
            '.' + SW_TS + '-svc-ibtn:hover{color:#ff3b30;background:rgba(255,59,48,0.08)}',
            '.' + SW_TS + '-trange-arrow{flex:0 0 auto;align-self:flex-end;margin:0 0 7px;padding:0;color:#c7c7cc;font-size:13px;user-select:none}',
            '.' + SW_TS + '-qchips-label{flex:0 0 auto;align-self:center;font-size:12px;font-weight:600;color:#86868b}',
            '.' + SW_TS + '-qchips{display:flex;gap:4px;flex-wrap:wrap;align-items:center;align-self:center;flex:0 1 auto;min-width:0;max-width:100%}',
            '.' + SW_TS + '-qactions{display:inline-flex;align-items:center;gap:6px;justify-self:end;align-self:end}',
            '.' + SW_TS + '-qbtn{padding:7px 16px;border:none;border-radius:9px;font-size:12.5px;font-weight:600;cursor:pointer;background:linear-gradient(135deg,#ff4b3d 0%,#ff7a35 100%);color:#fff;box-shadow:0 1px 2px rgba(255,59,48,0.28),0 6px 14px rgba(255,59,48,0.2),inset 0 1px 0 rgba(255,255,255,0.32);transition:box-shadow 0.2s,transform 0.2s,filter 0.2s;display:flex;align-items:center;gap:5px;height:32px;white-space:nowrap}',
            '.' + SW_TS + '-qbtn:hover{transform:translateY(-1px);filter:saturate(106%);box-shadow:0 2px 4px rgba(255,59,48,0.3),0 10px 22px rgba(255,59,48,0.26),inset 0 1px 0 rgba(255,255,255,0.36)}',
            '.' + SW_TS + '-qbtn:active{transform:translateY(0)}',
            '.' + SW_TS + '-qbtn:disabled{opacity:0.6;cursor:not-allowed;transform:none}',
            '.' + SW_TS + '-qbtn.sec{background:linear-gradient(180deg,rgba(255,255,255,0.96) 0%,rgba(255,255,255,0.8) 100%);color:#1d1d1f;border:1px solid rgba(0,0,0,0.075);box-shadow:0 1px 2px rgba(16,24,40,0.045),inset 0 1px 0 #fff}',
            '.' + SW_TS + '-qbtn.sec:hover{background:#fff;color:#d70015;border-color:rgba(255,59,48,0.28);box-shadow:0 2px 8px rgba(255,59,48,0.1),inset 0 1px 0 #fff;transform:translateY(-1px)}',
            '.' + SW_TS + '-autobox{display:inline-flex;align-items:center;gap:6px;padding:5px 8px;font-size:12px;color:#76767c;cursor:pointer;user-select:none;border-radius:8px;transition:background .15s ease,color .15s ease;white-space:nowrap}',
            '.' + SW_TS + '-autobox:hover{background:rgba(0,0,0,0.035);color:#1d1d1f}',
            '.' + SW_TS + '-autobox input{-webkit-appearance:none;appearance:none;width:28px;height:16px;margin:0;cursor:pointer;border-radius:999px;background:rgba(0,0,0,0.14);position:relative;transition:background 0.15s;outline:none;flex-shrink:0;box-shadow:inset 0 1px 2px rgba(16,24,40,0.12)}',
            '.' + SW_TS + '-autobox input:checked{background:#2fb350;box-shadow:inset 0 1px 1px rgba(0,0,0,0.08)}',
            '.' + SW_TS + '-autobox input::after{content:"";position:absolute;top:2px;left:2px;width:12px;height:12px;border-radius:50%;background:#fff;box-shadow:0 1px 2px rgba(0,0,0,0.22);transition:transform 0.15s}',
            '.' + SW_TS + '-autobox input:checked::after{transform:translateX(12px)}',
            '.' + SW_TS + '-qchip{padding:3px 8px;border:1px solid rgba(0,0,0,0.07);background:linear-gradient(180deg,rgba(255,255,255,0.9),rgba(255,255,255,0.68));border-radius:7px;font-size:10.5px;color:#c0392b;cursor:pointer;transition:background 0.15s,border-color 0.15s,color 0.15s;white-space:nowrap;box-shadow:0 1px 1px rgba(16,24,40,0.03),inset 0 1px 0 #fff}',
            '.' + SW_TS + '-qchip:hover{background:rgba(255,59,48,0.08);border-color:rgba(255,59,48,0.26);color:#d70015}',
            '.' + SW_TS + '-ikw-save{flex:0 0 auto;padding:0 9px;display:flex;align-items:center;border:1px solid rgba(0,0,0,0.075);background:linear-gradient(180deg,rgba(255,255,255,0.94) 0%,rgba(255,255,255,0.78) 100%);border-radius:8px;font-size:13px;color:#c87800;cursor:pointer;transition:background 0.15s,color 0.15s,border-color 0.15s;box-shadow:0 1px 2px rgba(16,24,40,0.04),inset 0 1px 0 #fff}',
            '.' + SW_TS + '-ikw-save:hover{background:rgba(255,149,0,0.1);border-color:rgba(255,149,0,0.34);color:#a86500}',
            '.' + SW_TS + '-ikw-pick-wrap{flex-shrink:0}',
            '.' + SW_TS + '-ikw-dd{position:relative;flex:0 0 86px;width:86px}',
            '.' + SW_TS + '-ikw-dd-btn{width:100%;padding:6px 20px 6px 8px;border:1px solid rgba(0,0,0,0.1);border-radius:7px;font-size:11.5px;background:rgba(255,255,255,0.8);color:#1d1d1f;outline:none;font-family:inherit;cursor:pointer;text-align:left;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;background-image:url("data:image/svg+xml,%3csvg xmlns=\'http://www.w3.org/2000/svg\' fill=\'none\' viewBox=\'0 0 20 20\'%3e%3cpath stroke=\'%2386868b\' stroke-linecap=\'round\' stroke-linejoin=\'round\' stroke-width=\'1.5\' d=\'m6 8 4 4 4-4\'/%3e%3c/svg%3e");background-repeat:no-repeat;background-position:right 6px center;background-size:11px}',
            '.' + SW_TS + '-ikw-dd-btn:hover{border-color:rgba(0,122,255,0.35);background:rgba(0,122,255,0.04)}',
            '.' + SW_TS + '-ikw-dd-btn:disabled{opacity:0.5;cursor:not-allowed;color:#86868b}',
            '.' + SW_TS + '-ikw-dd-menu{display:none;min-width:300px;max-width:420px;max-height:280px;overflow-x:hidden;overflow-y:auto;overscroll-behavior:contain;-webkit-overflow-scrolling:touch;background:rgba(255,255,255,0.98);backdrop-filter:blur(20px);-webkit-backdrop-filter:blur(20px);border:1px solid rgba(0,0,0,0.08);border-radius:10px;box-shadow:0 12px 36px rgba(0,0,0,0.16);z-index:100010;padding:4px}',
            '.' + SW_TS + '-ikw-dd-menu.open{display:block}',
            '.' + SW_TS + '-ikw-dd-item{display:flex;align-items:center;gap:6px;padding:6px 8px;border-radius:7px;transition:background 0.12s;cursor:pointer}',
            '.' + SW_TS + '-ikw-dd-item:hover{background:rgba(0,122,255,0.08)}',
            '.' + SW_TS + '-ikw-dd-text{flex:1;min-width:0;font-size:12px;color:#1d1d1f;cursor:pointer;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:"SF Mono","Menlo",monospace}',
            '.' + SW_TS + '-ikw-dd-del{flex:0 0 22px;width:22px;height:22px;border:none;background:rgba(0,0,0,0.04);border-radius:5px;cursor:pointer;font-size:14px;color:#86868b;display:flex;align-items:center;justify-content:center;transition:all 0.12s;padding:0}',
            '.' + SW_TS + '-ikw-dd-del:hover{background:rgba(255,59,48,0.12);color:#ff3b30}',

            // Stats
            '.' + SW_TS + '-stats{display:flex;align-items:center;gap:14px;flex-wrap:wrap;padding:0 4px;font-size:12px;color:#76767c;flex-shrink:0}',
            '.' + SW_TS + '-stats b{color:#1d1d1f;font-weight:600}',
            '.' + SW_TS + '-stats .ok{color:#1f8a3d;font-weight:600}',
            '.' + SW_TS + '-stats .warn{color:#c87800;font-weight:600}',

            // Log list (card-based)
            '.' + SW_TS + '-list{flex:1;min-height:0;overflow:auto;background:linear-gradient(180deg,rgba(255,255,255,0.8) 0%,rgba(255,255,255,0.62) 100%);backdrop-filter:blur(30px) saturate(160%);-webkit-backdrop-filter:blur(30px) saturate(160%);border:1px solid rgba(0,0,0,0.05);border-radius:16px;box-shadow:0 1px 2px rgba(16,24,40,0.03),0 18px 44px rgba(16,24,40,0.075),inset 0 1px 0 rgba(255,255,255,0.9);position:relative;padding:0px}',
            // Tab content 容器：每个 tab 独立 DOM，切换 tab 只切 display，不重渲染
            '.' + SW_TS + '-tab-content{display:block;min-height:100%}',
            // 时间标签：双击可填入查询
            '.' + SW_TS + '-item-time{font-size:12px;font-weight:500;color:#1d1d1f;font-variant-numeric:tabular-nums;flex-shrink:0;cursor:pointer;user-select:none}',
            '.' + SW_TS + '-item-time:hover{background:rgba(0,122,255,0.1);border-radius:4px}',
            '.' + SW_TS + '-item{padding:10px 14px;background:linear-gradient(180deg,rgba(255,255,255,0.72) 0%,rgba(255,255,255,0.5) 100%);border:1px solid rgba(0,0,0,0.055);border-radius:11px;margin:6px;transition:background 0.15s,border-color 0.15s,box-shadow 0.15s;box-shadow:0 1px 1px rgba(16,24,40,0.025),inset 0 1px 0 rgba(255,255,255,0.85)}',
            // 按日志级别着色：WARN 淡黄 / ERROR 淡红 / DEBUG 淡紫（渐变 + 3px 左侧色条；hover 规则必须带上 box-shadow，否则等权重下被 .item:hover 抹掉）
            '.' + SW_TS + '-item.lv-bg-WARN{background:linear-gradient(180deg,rgba(255,204,0,0.16) 0%,rgba(255,204,0,0.06) 100%);border-color:rgba(230,170,0,0.3);box-shadow:inset 3px 0 0 rgba(240,170,0,0.85),0 1px 1px rgba(16,24,40,0.025),inset 0 1px 0 rgba(255,255,255,0.6)}',
            '.' + SW_TS + '-item.lv-bg-WARN:hover{background:linear-gradient(180deg,rgba(255,204,0,0.24) 0%,rgba(255,204,0,0.1) 100%);border-color:rgba(230,170,0,0.45);box-shadow:inset 3px 0 0 rgba(240,170,0,0.9),0 4px 12px rgba(16,24,40,0.05),inset 0 1px 0 rgba(255,255,255,0.6)}',
            '.' + SW_TS + '-item.lv-bg-ERROR{background:linear-gradient(180deg,rgba(255,59,48,0.14) 0%,rgba(255,59,48,0.05) 100%);border-color:rgba(255,59,48,0.28);box-shadow:inset 3px 0 0 rgba(230,42,30,0.9),0 1px 1px rgba(16,24,40,0.025),inset 0 1px 0 rgba(255,255,255,0.6)}',
            '.' + SW_TS + '-item.lv-bg-ERROR:hover{background:linear-gradient(180deg,rgba(255,59,48,0.22) 0%,rgba(255,59,48,0.09) 100%);border-color:rgba(255,59,48,0.44);box-shadow:inset 3px 0 0 rgba(230,42,30,0.95),0 4px 12px rgba(16,24,40,0.05),inset 0 1px 0 rgba(255,255,255,0.6)}',
            '.' + SW_TS + '-item.lv-bg-DEBUG{background:linear-gradient(180deg,rgba(175,82,222,0.14) 0%,rgba(175,82,222,0.05) 100%);border-color:rgba(150,60,200,0.28);box-shadow:inset 3px 0 0 rgba(150,60,200,0.85),0 1px 1px rgba(16,24,40,0.025),inset 0 1px 0 rgba(255,255,255,0.6)}',
            '.' + SW_TS + '-item.lv-bg-DEBUG:hover{background:linear-gradient(180deg,rgba(175,82,222,0.22) 0%,rgba(175,82,222,0.09) 100%);border-color:rgba(150,60,200,0.44);box-shadow:inset 3px 0 0 rgba(150,60,200,0.9),0 4px 12px rgba(16,24,40,0.05),inset 0 1px 0 rgba(255,255,255,0.6)}',
            '.' + SW_TS + '-item:hover{background:linear-gradient(180deg,#fff 0%,rgba(255,255,255,0.86) 100%);border-color:rgba(0,0,0,0.1);box-shadow:0 4px 14px rgba(16,24,40,0.06),inset 0 1px 0 #fff}',
            '.' + SW_TS + '-item-head{display:flex;align-items:center;gap:6px;flex-wrap:wrap;margin-bottom:6px}',
            '.' + SW_TS + '-item-no{font-size:11px;font-weight:700;color:#76767c;background:rgba(0,0,0,0.045);padding:2px 7px;border-radius:6px;font-variant-numeric:tabular-nums;flex-shrink:0}',
            '.' + SW_TS + '-item-time{font-size:12px;font-weight:500;color:#1d1d1f;font-variant-numeric:tabular-nums;flex-shrink:0}',
            '.' + SW_TS + '-item-tag{font-size:11px;padding:2px 8px;border-radius:6px;font-weight:500;white-space:nowrap;flex-shrink:0;max-width:240px;overflow:hidden;text-overflow:ellipsis}',
            '.' + SW_TS + '-item-tag.lv-INFO{background:rgba(0,122,255,0.1);color:#007aff;border:1px solid rgba(0,122,255,0.15)}',
            '.' + SW_TS + '-item-tag.lv-WARN{background:rgba(255,149,0,0.12);color:#b25e00;border:1px solid rgba(255,149,0,0.2)}',
            '.' + SW_TS + '-item-tag.lv-ERROR{background:rgba(255,59,48,0.12);color:#c8231a;border:1px solid rgba(255,59,48,0.2)}',
            '.' + SW_TS + '-item-tag.lv-DEBUG{background:rgba(175,82,222,0.12);color:#7a3099;border:1px solid rgba(175,82,222,0.2)}',
            '.' + SW_TS + '-item-tag.svc{background:rgba(255,255,255,0.72);color:#1d1d1f;border:1px solid rgba(0,0,0,0.06)}',
            '.' + SW_TS + '-item-tag.inst{background:rgba(255,255,255,0.6);color:#76767c;border:1px solid rgba(0,0,0,0.055)}',
            '.' + SW_TS + '-item-tag.ep{background:rgba(88,86,214,0.1);color:#4a48b8;border:1px solid rgba(88,86,214,0.22);font-family:"SF Mono","Menlo",monospace;font-size:10.5px}',
            '.' + SW_TS + '-item-tag.trace{font-family:"SF Mono","Menlo",monospace;font-size:10.5px;background:rgba(0,122,255,0.08);color:#007aff;border:1px solid rgba(0,122,255,0.15);cursor:pointer;transition:all 0.15s;max-width:none;overflow:visible;text-overflow:clip}',
            '.' + SW_TS + '-item-tag.trace:hover{background:rgba(0,122,255,0.18);border-color:rgba(0,122,255,0.4)}',
            '.' + SW_TS + '-item-tag.logger{background:rgba(255,149,0,0.08);color:#b25e00;border:1px solid rgba(255,149,0,0.18);max-width:180px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:"SF Mono","Menlo",monospace;font-size:10.5px;cursor:help}',
            '.' + SW_TS + '-item-tag.thread{background:rgba(88,86,214,0.09);color:#4a48b8;border:1px solid rgba(88,86,214,0.2);max-width:140px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:"SF Mono","Menlo",monospace;font-size:10.5px;cursor:help}',
            '.' + SW_TS + '-item-tag.go{background:rgba(255,59,48,0.08);color:#ff3b30;border:1px solid rgba(255,59,48,0.18);cursor:pointer;padding:2px 7px;display:inline-flex;align-items:center;transition:all 0.15s}',
            '.' + SW_TS + '-item-tag.go:hover{background:rgba(255,59,48,0.18);border-color:rgba(255,59,48,0.4);transform:scale(1.05)}',
            '.' + SW_TS + '-item-body{font-family:"SF Mono","Menlo","Monaco","Consolas",monospace;font-size:12px;line-height:1.55;color:#1d1d1f;background:rgba(255,255,255,0.6);padding:8px 10px;border-radius:8px;white-space:pre-wrap;word-break:break-word;max-height:140px;overflow:hidden;position:relative;border-left:2px solid rgba(0,0,0,0.09);cursor:pointer;box-shadow:inset 0 1px 2px rgba(16,24,40,0.025)}',
            // 正文左边框跟随级别（INFO/未分级保持中性灰，不再一律标红）
            '.' + SW_TS + '-item.lv-bg-WARN .' + SW_TS + '-item-body{border-left-color:rgba(240,170,0,0.65)}',
            '.' + SW_TS + '-item.lv-bg-ERROR .' + SW_TS + '-item-body{border-left-color:rgba(230,42,30,0.6)}',
            '.' + SW_TS + '-item.lv-bg-DEBUG .' + SW_TS + '-item-body{border-left-color:rgba(150,60,200,0.6)}',
            '.' + SW_TS + '-item-body.exp{max-height:none}',
            '.' + SW_TS + '-item-body-mask{position:absolute;left:0;right:0;bottom:0;height:36px;background:linear-gradient(180deg,rgba(255,255,255,0) 0%,rgba(252,252,253,0.96) 80%);pointer-events:none;border-radius:0 0 6px 6px}',
            '.' + SW_TS + '-item-toggle{position:absolute;right:6px;bottom:4px;background:rgba(0,122,255,0.08);color:#007aff;border:1px solid rgba(0,122,255,0.2);border-radius:5px;padding:1px 7px;font-size:10px;cursor:pointer;font-family:-apple-system,sans-serif;line-height:1.4;z-index:2;transition:background 0.15s,border-color 0.15s}',
            '.' + SW_TS + '-item-toggle:hover{background:rgba(0,122,255,0.18);border-color:rgba(0,122,255,0.4)}',
            '.' + SW_TS + '-item-extra{display:flex;gap:6px;flex-wrap:wrap;margin-top:6px;font-family:"SF Mono","Menlo",monospace;font-size:10.5px}',
            '.' + SW_TS + '-item-extra span{background:rgba(255,255,255,0.66);padding:1px 6px;border-radius:5px;color:#76767c;border:1px solid rgba(0,0,0,0.05)}',

            '#' + SW_TS + '-wrap mark,#' + SW_TS + '-modal mark{background:rgba(255,204,0,0.4);color:#1d1d1f;padding:0 2px;border-radius:2px;font-weight:600}',
            '.' + SW_TS + '-empty{padding:60px 20px;text-align:center;color:#86868b;font-size:13px}',
            '.' + SW_TS + '-empty .ic{font-size:36px;margin-bottom:8px;opacity:0.4}',
            '.' + SW_TS + '-loadbar{position:sticky;top:0;left:0;right:0;height:3px;background:linear-gradient(90deg,#ff3b30,#ff9500,#ffcc00);background-size:200% 100%;animation:' + SW_TS + '-ld 1.5s ease-in-out infinite;border-radius:14px 14px 0 0;display:none;z-index:50;margin:-4px -4px 4px}',
            '.' + SW_TS + '-loadbar.on{display:block}',
            '@keyframes ' + SW_TS + '-ld{0%{background-position:200% 0}100%{background-position:-200% 0}}',
            '.' + SW_TS + '-toast{position:fixed;top:24px;left:50%;transform:translateX(-50%);background:rgba(0,0,0,0.8);backdrop-filter:blur(20px);-webkit-backdrop-filter:blur(20px);color:#fff;padding:8px 18px;border-radius:10px;font-size:13px;z-index:100000;box-shadow:0 8px 24px rgba(0,0,0,0.3);border:1px solid rgba(255,255,255,0.1);opacity:0;transition:opacity 0.2s;font-family:-apple-system,sans-serif}',
            '.' + SW_TS + '-toast.show{opacity:1}',

            // JSON syntax
            '#' + SW_TS + '-modal .sw-json-key{color:#c8231a;font-weight:600}',
            '#' + SW_TS + '-modal .sw-json-str{color:#b25e00}',
            '#' + SW_TS + '-modal .sw-json-num{color:#007aff}',
            '#' + SW_TS + '-modal .sw-json-bool{color:#5856d6;font-weight:600}',
            '#' + SW_TS + '-modal .sw-json-null{color:#86868b;font-style:italic}',

            // Resource contention diagnosis
            '.' + SW_TS + '-diag-trigger{border:0;background:linear-gradient(135deg,#ff3b30,#ff7a00);color:#fff;border-radius:10px;padding:8px 13px;font-size:12px;font-weight:700;cursor:pointer;box-shadow:0 6px 18px rgba(255,59,48,0.28);transition:transform .18s ease,opacity .18s ease;display:flex;align-items:center;gap:6px;font-family:-apple-system,sans-serif}',
            '.' + SW_TS + '-diag-trigger:hover{transform:translateY(-1px)}',
            '.' + SW_TS + '-diag-trigger:disabled{opacity:.62;cursor:wait;transform:none}',
            '.' + SW_TS + '-diag-trigger.' + SW_TS + '-diag-compact{height:32px;padding:0 12px;border-radius:8px;box-shadow:0 1px 4px rgba(0,0,0,.06);flex:0 0 auto;align-self:center}',
            '.' + SW_TS + '-diag-info-btn{width:28px;height:28px;border-radius:50%;border:1px solid rgba(0,0,0,.1);background:rgba(255,255,255,.48);color:#86868b;font-size:15px;font-weight:700;cursor:pointer;display:inline-flex;align-items:center;justify-content:center;align-self:center;transition:all .15s}',
            '.' + SW_TS + '-diag-info-btn:hover{background:rgba(0,0,0,.05);color:#6e6e73;transform:translateY(-1px)}',
            '#' + SW_TS + '-btn:hover{box-shadow:0 9px 26px rgba(255,59,48,0.42), inset 0 1px 0 rgba(255,255,255,0.25)}',
            '#' + SW_TS + '-btn:active{transform:scale(0.97)}',
            '#' + SW_TS + '-diag-modal{position:fixed;inset:0;z-index:100020;background:rgba(8,10,16,.68);backdrop-filter:blur(14px);-webkit-backdrop-filter:blur(14px);display:none;align-items:center;justify-content:center;padding:24px;font-family:-apple-system,BlinkMacSystemFont,"SF Pro Text",sans-serif;color:#f5f5f7}',
            '#' + SW_TS + '-diag-panel{width:min(1180px,96vw);max-height:92vh;overflow:auto;background:linear-gradient(145deg,rgba(36,38,46,.97),rgba(16,18,24,.98));border:1px solid rgba(255,255,255,.13);border-radius:22px;box-shadow:0 30px 100px rgba(0,0,0,.5);position:relative}',
            '.' + SW_TS + '-diag-head{padding:22px 58px 17px 24px;border-bottom:1px solid rgba(255,255,255,.09)}',
            '.' + SW_TS + '-diag-title{font-size:20px;font-weight:800;letter-spacing:-.3px}',
            '.' + SW_TS + '-diag-sub{font-size:12px;color:#a9abb4;margin-top:5px;line-height:1.5}',
            '.' + SW_TS + '-diag-close{position:absolute;right:17px;top:17px;width:34px;height:34px;border-radius:50%;border:1px solid rgba(255,255,255,.14);background:rgba(255,255,255,.08);color:#fff;font-size:17px;cursor:pointer}',
            '.' + SW_TS + '-diag-content{padding:18px 20px 22px}',
            '.' + SW_TS + '-diag-grid{display:grid;grid-template-columns:1.08fr 1fr 1fr;gap:13px}',
            '.' + SW_TS + '-diag-card{background:rgba(255,255,255,.055);border:1px solid rgba(255,255,255,.1);border-radius:16px;padding:16px;min-width:0}',
            '.' + SW_TS + '-diag-card h3{margin:0 0 12px;font-size:14px;display:flex;align-items:center;gap:7px}',
            '.' + SW_TS + '-diag-item{padding:10px 0;border-top:1px solid rgba(255,255,255,.075)}',
            '.' + SW_TS + '-diag-item:first-of-type{border-top:0;padding-top:0}',
            '.' + SW_TS + '-diag-name{font-size:12px;font-weight:700;word-break:break-all;line-height:1.45}',
            '.' + SW_TS + '-diag-meta{font-size:11px;color:#b5b7c0;line-height:1.65;margin-top:5px}',
            '.' + SW_TS + '-diag-pill{display:inline-block;padding:2px 7px;border-radius:999px;font-size:10px;font-weight:800;margin-right:5px}',
            '.' + SW_TS + '-diag-high{background:rgba(255,59,48,.2);color:#ff8179}',
            '.' + SW_TS + '-diag-medium{background:rgba(255,204,0,.18);color:#ffd84d}',
            '.' + SW_TS + '-diag-low{background:rgba(142,142,147,.2);color:#c7c7cc}',
            '.' + SW_TS + '-diag-ok{background:rgba(52,199,89,.16);color:#70dc8a}',
            '.' + SW_TS + '-diag-warn{margin-top:13px;padding:10px 12px;border-radius:11px;background:rgba(255,149,0,.1);border:1px solid rgba(255,149,0,.16);font-size:11px;color:#ffc069;line-height:1.55}',
            '.' + SW_TS + '-diag-loading{padding:68px 20px;text-align:center;color:#c7c7cc}',
            '.' + SW_TS + '-diag-spinner{width:34px;height:34px;border:3px solid rgba(255,255,255,.16);border-top-color:#ff5b4d;border-radius:50%;margin:0 auto 14px;animation:' + SW_TS + '-spin .8s linear infinite}',
            '@keyframes ' + SW_TS + '-spin{to{transform:rotate(360deg)}}',
            '.' + SW_TS + '-diag-form{display:grid;grid-template-columns:1.4fr 1fr 1fr auto;gap:9px;align-items:end}',
            '.' + SW_TS + '-diag-form label{font-size:11px;color:#b5b7c0;display:flex;flex-direction:column;gap:5px}',
            '.' + SW_TS + '-diag-form input{border:1px solid rgba(255,255,255,.14);background:rgba(255,255,255,.08);color:#fff;border-radius:9px;padding:9px 10px;outline:none}',
            '@media(max-width:820px){.' + SW_TS + '-diag-grid{grid-template-columns:1fr}.' + SW_TS + '-diag-form{grid-template-columns:1fr}#' + SW_TS + '-diag-modal{padding:10px}}',

            // Modal
            '#' + SW_TS + '-modal{position:fixed;inset:0;background:rgba(0,0,0,0.45);backdrop-filter:blur(8px);-webkit-backdrop-filter:blur(8px);z-index:100001;display:none;align-items:center;justify-content:center;padding:20px;animation:' + SW_TS + '-mfade 0.2s ease}',
            '@keyframes ' + SW_TS + '-mfade{from{opacity:0}to{opacity:1}}',
            '#' + SW_TS + '-modal-body{width:95vw;max-width:95vw;height:95vh;max-height:95vh;margin:0;background:linear-gradient(180deg,rgba(255,255,255,0.95) 0%,rgba(255,255,255,0.9) 100%);backdrop-filter:blur(40px) saturate(180%);-webkit-backdrop-filter:blur(40px) saturate(180%);border:1px solid rgba(0,0,0,0.06);border-radius:18px;box-shadow:0 2px 8px rgba(16,24,40,0.06),0 30px 80px rgba(16,24,40,0.18),inset 0 1px 0 rgba(255,255,255,0.98);display:flex;flex-direction:column;overflow:hidden;position:relative;font-family:-apple-system,sans-serif;transition:opacity 0.25s ease,transform 0.25s ease}',
            '.' + SW_TS + '-modal-tabs{display:flex;gap:6px;padding:10px 50px 10px 14px;border-bottom:1px solid rgba(0,0,0,0.06);background:linear-gradient(180deg,rgba(255,255,255,0.6) 0%,rgba(248,248,251,0.42) 100%);flex-wrap:wrap;align-items:center}',
            '.' + SW_TS + '-mtab{padding:6px 14px;border:1px solid rgba(0,0,0,0.07);background:linear-gradient(180deg,rgba(255,255,255,0.94) 0%,rgba(255,255,255,0.72) 100%);border-radius:8px;font-size:12.5px;cursor:pointer;color:#3a3a3c;transition:background 0.15s,border-color 0.15s,color 0.15s,box-shadow 0.15s;box-shadow:0 1px 2px rgba(16,24,40,0.035),inset 0 1px 0 #fff}',
            '.' + SW_TS + '-mtab:hover{background:#fff;color:#1d1d1f;border-color:rgba(0,0,0,0.12)}',
            '.' + SW_TS + '-mtab.active{background:linear-gradient(135deg,#ff4b3d,#ff7a35);color:#fff;border-color:transparent;font-weight:600;box-shadow:0 1px 2px rgba(255,59,48,0.26),0 5px 14px rgba(255,59,48,0.22),inset 0 1px 0 rgba(255,255,255,0.3)}',
            '.' + SW_TS + '-modal-x{position:absolute;top:14px;right:14px;width:32px;height:32px;background:linear-gradient(180deg,rgba(255,255,255,0.98) 0%,rgba(255,255,255,0.86) 100%);border:1px solid rgba(0,0,0,0.08);border-radius:50%;cursor:pointer;font-size:16px;color:#3a3a3c;display:flex;align-items:center;justify-content:center;transition:background 0.15s,color 0.15s,border-color 0.15s;z-index:10;box-shadow:0 2px 8px rgba(16,24,40,0.1),inset 0 1px 0 #fff;font-weight:600}',
            '.' + SW_TS + '-modal-x:hover{background:rgba(255,59,48,0.1);color:#d70015;border-color:rgba(255,59,48,0.24)}',
            '.' + SW_TS + '-modal-view{flex:1;min-height:0;overflow:auto;padding:14px 16px;display:block}',
            '.' + SW_TS + '-modal-pre{font-family:"SF Mono","Menlo","Monaco",monospace;font-size:12px;line-height:1.6;color:#1d1d1f;white-space:pre-wrap;word-break:break-word;margin:0}',
            '.' + SW_TS + '-modal-search{width:100%;padding:8px 12px;border:1px solid rgba(0,0,0,0.085);border-radius:9px;font-size:13px;margin-bottom:10px;outline:none;background:linear-gradient(180deg,#fff 0%,#fcfcfd 100%);color:#1d1d1f;transition:border-color 0.2s,box-shadow 0.2s;box-shadow:0 1px 1px rgba(16,24,40,0.025),inset 0 1px 1px rgba(16,24,40,0.02)}',
            '.' + SW_TS + '-modal-search:focus{border-color:rgba(255,59,48,0.45);box-shadow:0 0 0 3px rgba(255,59,48,0.12)}',
            '.' + SW_TS + '-modal-search-result{padding:8px;background:rgba(255,255,255,0.62);border:1px solid rgba(0,0,0,0.05);border-radius:9px;min-height:60px}',

            // Trace view
            '.' + SW_TS + '-trace-loading{padding:40px;text-align:center;color:#86868b;font-size:13px}',
            '.' + SW_TS + '-trace-summary{margin-bottom:12px}',
            '.' + SW_TS + '-ts-row{display:flex;gap:8px;flex-wrap:wrap;align-items:center}',
            '.' + SW_TS + '-ts-pill{font-size:11.5px;padding:4px 10px;background:linear-gradient(180deg,rgba(255,255,255,0.95) 0%,rgba(255,255,255,0.75) 100%);border:1px solid rgba(0,0,0,0.06);border-radius:8px;color:#1d1d1f;font-weight:500;box-shadow:0 1px 2px rgba(16,24,40,0.03),inset 0 1px 0 #fff}',
            '.' + SW_TS + '-ts-err{background:rgba(255,59,48,0.1);color:#c8231a;border-color:rgba(255,59,48,0.2)}',
            '.' + SW_TS + '-ts-ok{background:rgba(52,199,89,0.1);color:#1e7a35;border-color:rgba(52,199,89,0.2)}',
            '.' + SW_TS + '-trace-legend{display:flex;gap:10px;flex-wrap:wrap;padding:8px 0;margin-bottom:8px;font-size:11px;color:#86868b}',
            '.' + SW_TS + '-tn-legend-item{display:flex;align-items:center;gap:4px}',
            '.' + SW_TS + '-trace-tree{background:linear-gradient(180deg,rgba(255,255,255,0.78) 0%,rgba(255,255,255,0.55) 100%);border:1px solid rgba(0,0,0,0.055);border-radius:12px;padding:6px;overflow-x:auto;box-shadow:0 1px 2px rgba(16,24,40,0.03),inset 0 1px 0 rgba(255,255,255,0.9)}',
            '.' + SW_TS + '-tn{position:relative;padding:0;border-bottom:1px solid rgba(0,0,0,0.04);display:grid;grid-template-columns:auto minmax(260px,1.2fr) 2fr 60px;gap:0;align-items:stretch;min-width:fit-content;transition:background 0.15s}',
            '.' + SW_TS + '-tn:hover{background:rgba(255,59,48,0.04)}',
            '.' + SW_TS + '-tn-err{background:rgba(255,59,48,0.06)}',
            '.' + SW_TS + '-tn-tree{flex:0 0 auto;display:flex;align-items:stretch;font-family:"SF Mono","Menlo",monospace;color:#b0b0b5;font-size:13px;line-height:1;user-select:none;align-self:stretch;padding:8px 0 8px 4px}',
            '.' + SW_TS + '-tn-tree-col{position:relative;width:16px;flex:0 0 16px}',
            '.' + SW_TS + '-tn-tree-col::before{content:"";position:absolute;left:50%;top:0;bottom:0;width:1px;background:transparent}',
            '.' + SW_TS + '-tn-tree-col.has-vert::before{background:#c8c8cc}',
            '.' + SW_TS + '-tn-tree-col.last-vert::before{background:linear-gradient(to bottom,#c8c8cc 50%,transparent 50%)}',
            '.' + SW_TS + '-tn-tree-col.connector{position:relative}',
            '.' + SW_TS + '-tn-tree-col.connector::after{content:"";position:absolute;left:50%;top:0;bottom:0;width:1px;background:transparent}',
            '.' + SW_TS + '-tn-tree-col.connector::before{content:"";position:absolute;left:50%;top:50%;width:8px;height:1px;background:#c8c8cc}',
            '.' + SW_TS + '-tn-head{display:flex;align-items:center;gap:6px;min-width:0;position:relative;padding:10px 8px 10px 0}',
            '.' + SW_TS + '-tn-dot{flex:0 0 10px;width:10px;height:10px;border-radius:50%;box-shadow:0 0 0 2px rgba(255,255,255,0.8)}',
            '.' + SW_TS + '-tn-svc{flex:0 0 auto;font-weight:600;font-size:12px;max-width:130px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}',
            '.' + SW_TS + '-tn-layer{flex:0 0 auto;font-size:10px;background:rgba(175,82,222,0.14);color:#7a3099;padding:1px 6px;border-radius:4px;font-weight:600}',
            '.' + SW_TS + '-tn-ep{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-family:"SF Mono",monospace;font-size:12px;color:#1d1d1f;min-width:0}',
            '.' + SW_TS + '-tn-sqlcopy{flex:0 0 auto;background:rgba(52,199,89,0.1);color:#34c759;border:1px solid rgba(52,199,89,0.25);cursor:pointer;padding:1px 6px;border-radius:4px;font-size:11px;transition:all 0.15s;display:inline-flex;align-items:center;line-height:1.3}',
            '.' + SW_TS + '-tn-sqlcopy:hover{background:rgba(52,199,89,0.22);border-color:rgba(52,199,89,0.45);transform:scale(1.05)}',
            '.' + SW_TS + '-tn-errpill{flex:0 0 auto;background:#ff3b30;color:#fff;padding:1px 6px;border-radius:4px;font-size:10px;font-weight:700}',
            '.' + SW_TS + '-tn-bar-wrap{position:relative;height:18px;margin:10px 0;background:rgba(16,24,40,0.045);border-radius:5px;overflow:hidden;align-self:center;min-width:180px;box-shadow:inset 0 1px 2px rgba(16,24,40,0.05)}',
            '.' + SW_TS + '-tn-track{position:absolute;inset:0;background-image:linear-gradient(to right,rgba(16,24,40,0.05) 1px,transparent 1px);background-size:10% 100%}',
            '.' + SW_TS + '-tn-bar{position:absolute;top:2px;bottom:2px;border-radius:3px;box-shadow:0 1px 2px rgba(0,0,0,0.15);min-width:2px;transition:opacity 0.15s ease}',
            '.' + SW_TS + '-tn:hover .' + SW_TS + '-tn-bar{opacity:0.85}',
            '.' + SW_TS + '-tn-dur{font-size:11.5px;color:#1d1d1f;text-align:right;font-variant-numeric:tabular-nums;font-weight:600;padding:10px 8px 10px 0;align-self:center;font-family:"SF Mono","Menlo",monospace}',
            '.' + SW_TS + '-tn-tags{grid-column:2 / -1;display:flex;gap:4px;flex-wrap:wrap;padding:0 10px 8px 0;margin-top:-2px}',
            '.' + SW_TS + '-tn-tag{font-size:10px;background:rgba(255,255,255,0.8);color:#3a3a3c;padding:1px 6px;border-radius:8px;white-space:nowrap;font-family:-apple-system,sans-serif;border:1px solid rgba(0,0,0,0.05)}',
            '.' + SW_TS + '-tn-tag b{color:#1d1d1f;font-weight:600}',

            // 浅色滚动条与选中色（深色块在其后，权重更高会覆盖）
            '#' + SW_TS + '-wrap ::-webkit-scrollbar,#' + SW_TS + '-modal ::-webkit-scrollbar{width:11px;height:11px}',
            '#' + SW_TS + '-wrap ::-webkit-scrollbar-track,#' + SW_TS + '-modal ::-webkit-scrollbar-track{background:transparent}',
            '#' + SW_TS + '-wrap ::-webkit-scrollbar-thumb,#' + SW_TS + '-modal ::-webkit-scrollbar-thumb{background:rgba(60,60,67,0.2);border:3px solid transparent;background-clip:padding-box;border-radius:8px}',
            '#' + SW_TS + '-wrap ::-webkit-scrollbar-thumb:hover,#' + SW_TS + '-modal ::-webkit-scrollbar-thumb:hover{background:rgba(60,60,67,0.36);background-clip:padding-box}',
            '#' + SW_TS + '-wrap ::selection,#' + SW_TS + '-modal ::selection{background:rgba(255,59,48,0.14)}',

            // ==================== Dark theme overrides（sw-dark 挂在面板与详情弹窗上） ====================
            // 深色色板 token：两个根（面板 / 详情弹窗）都要定义，子规则统一引用
            '#' + SW_TS + '-wrap.sw-dark,#' + SW_TS + '-modal.sw-dark{color-scheme:dark;--d-t0:#f2f3f7;--d-t1:#c9ccd4;--d-t2:#989ea9;--d-t3:#6a6f7b;--d-line:rgba(255,255,255,0.08);--d-line-2:rgba(255,255,255,0.14);--d-glass:#23262e;--d-el1:#22252d;--d-el2:#1d1f27;--d-el3:#181a21;--d-hi:0 0 0 0 transparent;--d-red:#ff4830;--d-amber:#ffab33;--d-blue:#57a8ff;--d-violet:#b98af0;--d-green:#5fd97a;--d-primary:linear-gradient(135deg,#e8452d 0%,#b8291b 100%);--d-primary-hover:linear-gradient(135deg,#f7573d 0%,#c9301f 100%)}',
            '#' + SW_TS + '-wrap.sw-dark{background:radial-gradient(1100px 520px at 12% -10%,rgba(255,72,48,0.07) 0%,transparent 62%),radial-gradient(900px 460px at 88% -6%,rgba(255,120,60,0.04) 0%,transparent 58%),linear-gradient(180deg,#1a1c23 0%,#0f1116 100%);color:var(--d-t0)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-header{background:rgba(31,34,42,0.86);border-bottom-color:rgba(255,255,255,0.07);box-shadow:0 1px 0 rgba(0,0,0,0.5),0 10px 28px rgba(0,0,0,0.28)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-logo{background:linear-gradient(135deg,#e8452d,#b8291b);box-shadow:0 2px 10px rgba(232,69,45,0.3)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-title span{color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-exit,#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-theme-btn{background:#22252d;border-color:var(--d-line);color:var(--d-t1);box-shadow:none}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-exit:hover,#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-theme-btn:hover{background:#2b2f39;border-color:var(--d-line-2);color:var(--d-t0)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-sub-btn{background:var(--d-glass);border-color:var(--d-line);color:var(--d-t1);box-shadow:none}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-sub-btn:hover{background:#2b2f39;color:var(--d-t0)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-sub-btn.active{background:#2f333d;border-color:rgba(255,72,48,0.5);color:var(--d-t0);box-shadow:0 0 0 1px rgba(255,72,48,0.12),0 4px 14px rgba(0,0,0,0.3)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-sub-btn .cs{color:var(--d-t3)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-sub-btn .cs:hover{background:rgba(255,72,48,0.2);color:#ff8b80}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-sub-btn .tab-label:hover{background:rgba(255,72,48,0.2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-sub-btn .tab-label.editing{background:rgba(12,14,18,0.9)}',
            '#' + SW_TS + '-wrap.sw-dark .tab-label-input{color:var(--d-t0)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-add-btn{background:rgba(255,255,255,0.03);border-color:rgba(255,255,255,0.16);color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-add-btn:hover{background:rgba(255,255,255,0.08);color:var(--d-t0);border-color:rgba(255,72,48,0.45)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-close-all-btn{background:rgba(255,72,48,0.13);border-color:rgba(255,72,48,0.32);color:#ff8b80}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-close-all-btn:hover{background:rgba(255,72,48,0.24)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-qcard{background:var(--d-el2);border-color:rgba(255,255,255,0.07);box-shadow:0 14px 36px rgba(0,0,0,0.42)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-fg label,#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-qchips-label{color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-fg input,#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-fg select,#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-dd-btn,#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-modal-search{background:rgba(8,10,14,0.42);border-color:var(--d-line);color:var(--d-t0);box-shadow:inset 0 1px 2px rgba(0,0,0,0.32)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-fg input:focus,#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-fg select:focus,#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-modal-search:focus{background:rgba(8,10,14,0.6);border-color:rgba(255,72,48,0.55);box-shadow:inset 0 1px 2px rgba(0,0,0,0.28),0 0 0 3px rgba(255,72,48,0.2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-fg input::placeholder{color:var(--d-t3)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-fg select{background-image:url("data:image/svg+xml,%3csvg xmlns=\'http://www.w3.org/2000/svg\' fill=\'none\' viewBox=\'0 0 20 20\'%3e%3cpath stroke=\'%23989ea9\' stroke-linecap=\'round\' stroke-linejoin=\'round\' stroke-width=\'1.5\' d=\'m6 8 4 4 4-4\'/%3e%3c/svg%3e");background-repeat:no-repeat;background-position:right 8px center;background-size:12px;padding-right:26px;cursor:pointer}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-dd-btn{background-image:url("data:image/svg+xml,%3csvg xmlns=\'http://www.w3.org/2000/svg\' fill=\'none\' viewBox=\'0 0 20 20\'%3e%3cpath stroke=\'%23989ea9\' stroke-linecap=\'round\' stroke-linejoin=\'round\' stroke-width=\'1.5\' d=\'m6 8 4 4 4-4\'/%3e%3c/svg%3e");background-repeat:no-repeat;background-position:right 6px center;background-size:11px}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-fg input[type="datetime-local"]::-webkit-calendar-picker-indicator{background:url("data:image/svg+xml,%3csvg xmlns=\'http://www.w3.org/2000/svg\' fill=\'none\' viewBox=\'0 0 24 24\'%3e%3cpath stroke=\'%23d6d9e0\' stroke-width=\'1.8\' stroke-linecap=\'round\' stroke-linejoin=\'round\' d=\'M4 7a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V7zM9 3v5M15 3v5M4 12h16\'/%3e%3c/svg%3e") center no-repeat;background-size:14px 14px}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-fg input[type="datetime-local"]::-webkit-calendar-picker-indicator:hover{background-color:rgba(255,255,255,0.14)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-svc-ibtn{color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-svc-ibtn:hover{color:#ff8b80;background:rgba(255,72,48,0.16)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-trange-arrow{color:var(--d-t3)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-qbtn{background:var(--d-primary);box-shadow:0 4px 14px rgba(232,69,45,0.24)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-qbtn:hover{background:var(--d-primary-hover);box-shadow:0 6px 18px rgba(232,69,45,0.32)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-diag-trigger{background:var(--d-primary);box-shadow:0 4px 14px rgba(232,69,45,0.24)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-diag-trigger:hover{background:var(--d-primary-hover);box-shadow:0 8px 20px rgba(232,69,45,0.32)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-mtab.active{background:var(--d-primary);box-shadow:0 3px 12px rgba(232,69,45,0.26)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-qbtn.sec{background:#2a2e37;color:var(--d-t0);border-color:var(--d-line-2);box-shadow:none}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-qbtn.sec:hover{background:rgba(255,72,48,0.16);color:#ff9c93;border-color:rgba(255,72,48,0.4);box-shadow:none}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-autobox{color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-autobox:hover{background:#262a33;color:var(--d-t0)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-autobox input{background:rgba(255,255,255,0.18)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-autobox input:checked{background:rgba(52,199,89,0.85)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-qchip{background:#24272f;color:#ffa396;border-color:var(--d-line);box-shadow:none}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-qchip:hover{background:rgba(232,69,45,0.18);border-color:rgba(232,69,45,0.36)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-save{background:#24272f;border-color:var(--d-line);color:var(--d-amber);box-shadow:none}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-save:hover{background:rgba(255,171,51,0.16);border-color:rgba(255,171,51,0.34)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-loadbar{background:linear-gradient(90deg,#b8291b,#e8452d 45%,#ff8a5c);background-size:200% 100%}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-dd-btn:not(:disabled):hover{background:rgba(8,10,14,0.6);border-color:rgba(87,168,255,0.42)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-dd-btn:disabled{color:var(--d-t3)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-dd-menu{background:#262a33;border-color:var(--d-line-2);box-shadow:0 18px 48px rgba(0,0,0,0.6)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-dd-item:hover{background:rgba(87,168,255,0.14)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-dd-text{color:var(--d-t0)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-dd-del{background:rgba(255,255,255,0.06);color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-ikw-dd-del:hover{background:rgba(255,72,48,0.24);color:#ff8b80}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-stats{color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-stats b{color:var(--d-t0)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-stats .ok{color:var(--d-green)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-stats .warn{color:var(--d-amber)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-list{background:var(--d-el3);border-color:rgba(255,255,255,0.06);box-shadow:0 18px 44px rgba(0,0,0,0.45)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item{background:var(--d-el1);border-color:rgba(255,255,255,0.06);box-shadow:none}',
            // 级别底色必须一起覆盖：深色基础 item 规则带 id 权重，会压过浅色的 .item.lv-bg-* 类规则
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item.lv-bg-WARN{background:linear-gradient(rgba(255,204,0,0.085),rgba(255,204,0,0.085)),var(--d-el1);border-color:rgba(255,204,0,0.24);box-shadow:inset 3px 0 0 rgba(255,204,0,0.75)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item.lv-bg-WARN:hover{background:linear-gradient(rgba(255,204,0,0.14),rgba(255,204,0,0.14)),#2b2f39;border-color:rgba(255,204,0,0.38);box-shadow:inset 3px 0 0 rgba(255,204,0,0.85),0 6px 18px rgba(0,0,0,0.36)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item.lv-bg-ERROR{background:linear-gradient(rgba(255,72,48,0.11),rgba(255,72,48,0.11)),var(--d-el1);border-color:rgba(255,72,48,0.28);box-shadow:inset 3px 0 0 rgba(255,72,48,0.85)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item.lv-bg-ERROR:hover{background:linear-gradient(rgba(255,72,48,0.17),rgba(255,72,48,0.17)),#2b2f39;border-color:rgba(255,72,48,0.42);box-shadow:inset 3px 0 0 rgba(255,72,48,0.95),0 6px 18px rgba(0,0,0,0.36)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item.lv-bg-DEBUG{background:linear-gradient(rgba(175,82,222,0.1),rgba(175,82,222,0.1)),var(--d-el1);border-color:rgba(175,82,222,0.25);box-shadow:inset 3px 0 0 rgba(175,82,222,0.8)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item.lv-bg-DEBUG:hover{background:linear-gradient(rgba(175,82,222,0.16),rgba(175,82,222,0.16)),#2b2f39;border-color:rgba(175,82,222,0.4);box-shadow:inset 3px 0 0 rgba(175,82,222,0.9),0 6px 18px rgba(0,0,0,0.36)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item:hover{background:#2b2f39;border-color:var(--d-line-2);box-shadow:0 6px 18px rgba(0,0,0,0.36)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item.lv-bg-WARN .' + SW_TS + '-item-body{border-left-color:rgba(255,204,0,0.5)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item.lv-bg-ERROR .' + SW_TS + '-item-body{border-left-color:rgba(255,72,48,0.55)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item.lv-bg-DEBUG .' + SW_TS + '-item-body{border-left-color:rgba(175,82,222,0.5)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-no{background:#24272f;color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-time{color:var(--d-t0)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-time:hover{background:rgba(87,168,255,0.18)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.lv-INFO{background:rgba(87,168,255,0.14);color:var(--d-blue);border-color:rgba(87,168,255,0.28)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.lv-WARN{background:rgba(255,171,51,0.14);color:var(--d-amber);border-color:rgba(255,171,51,0.3)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.lv-ERROR{background:rgba(255,72,48,0.16);color:#ff8b80;border-color:rgba(255,72,48,0.32)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.lv-DEBUG{background:rgba(185,138,240,0.14);color:var(--d-violet);border-color:rgba(185,138,240,0.3)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.svc{background:#24272f;color:var(--d-t1);border-color:rgba(255,255,255,0.08)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.inst{background:#21242b;color:var(--d-t2);border-color:rgba(255,255,255,0.07)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.ep{background:rgba(88,86,214,0.16);color:#a8a4f7;border-color:rgba(88,86,214,0.3)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.trace{background:rgba(87,168,255,0.13);color:var(--d-blue);border-color:rgba(87,168,255,0.28)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.trace:hover{background:rgba(87,168,255,0.24);border-color:rgba(87,168,255,0.45)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.logger{background:rgba(255,171,51,0.12);color:var(--d-amber);border-color:rgba(255,171,51,0.26)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.thread{background:rgba(88,86,214,0.13);color:#a8a4f7;border-color:rgba(88,86,214,0.26)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.go{background:rgba(255,72,48,0.13);color:#ff8b80;border-color:rgba(255,72,48,0.28)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-tag.go:hover{background:rgba(255,72,48,0.24);border-color:rgba(255,72,48,0.45)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-body{background:rgba(6,8,12,0.42);color:#dfe2e9;box-shadow:inset 0 1px 2px rgba(0,0,0,0.3)}',
            // INFO/未分级：正文左边框走中性灰，不再沿用浅色的红色（级别色条由 .lv-bg-* 规则覆盖，权重更高）
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item .' + SW_TS + '-item-body{border-left-color:rgba(255,255,255,0.13)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-body-mask{background:linear-gradient(180deg,rgba(16,18,23,0) 0%,rgba(16,18,23,0.94) 78%)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-toggle{background:rgba(87,168,255,0.14);color:var(--d-blue);border-color:rgba(87,168,255,0.28)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-toggle:hover{background:rgba(87,168,255,0.26);border-color:rgba(87,168,255,0.45)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-item-extra span{background:#24272f;color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark mark,#' + SW_TS + '-modal.sw-dark mark{background:rgba(255,204,0,0.26);color:#fff;box-shadow:0 0 0 1px rgba(255,204,0,0.34)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-empty{color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-diag-info-btn{background:rgba(255,255,255,0.06);border-color:var(--d-line);color:var(--d-t2)}',
            '#' + SW_TS + '-wrap.sw-dark .' + SW_TS + '-diag-info-btn:hover{background:rgba(255,255,255,0.13);color:var(--d-t0)}',
            // 详情弹窗（挂在 document.body 上，独立 sw-dark）
            '#' + SW_TS + '-modal.sw-dark{background:rgba(6,8,12,0.62)}',
            '#' + SW_TS + '-modal.sw-dark #' + SW_TS + '-modal-body{background:#22252d;border-color:rgba(255,255,255,0.08);box-shadow:0 34px 90px rgba(0,0,0,0.62)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-modal-tabs{background:#1c1f26;border-bottom-color:rgba(255,255,255,0.07)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-mtab{background:#24272f;border-color:var(--d-line);color:var(--d-t1);box-shadow:none}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-mtab:hover{background:#2b2f39;color:var(--d-t0)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-modal-x{background:#2a2e37;border-color:var(--d-line-2);color:var(--d-t1);box-shadow:0 4px 14px rgba(0,0,0,0.4)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-modal-pre{color:#dfe2e9}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-modal-search-result{background:rgba(6,8,12,0.4);box-shadow:inset 0 1px 2px rgba(0,0,0,0.3)}',
            '#' + SW_TS + '-modal.sw-dark .sw-json-key{color:#ff8b80}',
            '#' + SW_TS + '-modal.sw-dark .sw-json-str{color:var(--d-amber)}',
            '#' + SW_TS + '-modal.sw-dark .sw-json-num{color:var(--d-blue)}',
            '#' + SW_TS + '-modal.sw-dark .sw-json-bool{color:var(--d-violet)}',
            '#' + SW_TS + '-modal.sw-dark .sw-json-null{color:var(--d-t3)}',
            // 调用链视图（渲染在详情弹窗内）
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-trace-loading{color:var(--d-t2)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-ts-pill{background:#24272f;border-color:var(--d-line);color:var(--d-t1);box-shadow:none}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-ts-err{background:rgba(255,72,48,0.16);color:#ff8b80;border-color:rgba(255,72,48,0.3)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-ts-ok{background:rgba(52,199,89,0.14);color:var(--d-green);border-color:rgba(52,199,89,0.28)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-trace-legend{color:var(--d-t2)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-trace-tree{background:#1f222a;border-color:rgba(255,255,255,0.06);box-shadow:none}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn{border-bottom-color:rgba(255,255,255,0.05)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn:hover{background:rgba(255,72,48,0.09)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-err{background:rgba(255,72,48,0.11)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-tree{color:var(--d-t3)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-tree-col.has-vert::before{background:#3c3f48}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-tree-col.last-vert::before{background:linear-gradient(to bottom,#3c3f48 50%,transparent 50%)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-tree-col.connector::before{background:#3c3f48}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-dot{box-shadow:0 0 0 2px rgba(18,20,26,0.95),0 0 8px rgba(0,0,0,0.4)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-layer{background:rgba(185,138,240,0.16);color:var(--d-violet)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-ep{color:#dfe2e9}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-bar-wrap{background:rgba(6,8,12,0.4);box-shadow:inset 0 1px 2px rgba(0,0,0,0.32)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-track{background-image:linear-gradient(to right,rgba(255,255,255,0.055) 1px,transparent 1px)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-dur{color:var(--d-t0)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-tag{background:rgba(255,255,255,0.06);color:var(--d-t1)}',
            '#' + SW_TS + '-modal.sw-dark .' + SW_TS + '-tn-tag b{color:var(--d-t0)}',
            // 滚动条与选中色（深色下不再露出系统亮色条）
            '#' + SW_TS + '-wrap.sw-dark *::selection,#' + SW_TS + '-modal.sw-dark *::selection{background:rgba(255,72,48,0.34);color:#fff}',
            '#' + SW_TS + '-wrap.sw-dark ::-webkit-scrollbar,#' + SW_TS + '-modal.sw-dark ::-webkit-scrollbar{width:11px;height:11px}',
            '#' + SW_TS + '-wrap.sw-dark ::-webkit-scrollbar-track,#' + SW_TS + '-modal.sw-dark ::-webkit-scrollbar-track{background:transparent}',
            '#' + SW_TS + '-wrap.sw-dark ::-webkit-scrollbar-thumb,#' + SW_TS + '-modal.sw-dark ::-webkit-scrollbar-thumb{background:rgba(255,255,255,0.14);border:3px solid transparent;background-clip:padding-box;border-radius:8px}',
            '#' + SW_TS + '-wrap.sw-dark ::-webkit-scrollbar-thumb:hover,#' + SW_TS + '-modal.sw-dark ::-webkit-scrollbar-thumb:hover{background:rgba(255,255,255,0.26);background-clip:padding-box}',
            '#' + SW_TS + '-wrap.sw-dark,#' + SW_TS + '-modal.sw-dark{scrollbar-color:rgba(255,255,255,0.18) transparent}',
        ].join('');
        var s = document.createElement('style');
        s.id = SW_TS + '-style';
        s.textContent = css;
        document.head.appendChild(s);
    }

    // ==================== Toggle Button ====================
    const BTN_POS_KEY = 'sw_btn_pos_v1';
    function createToggleButton() {
        if (document.getElementById(SW_TS + '-btn')) return;
        var btn = document.createElement('button');
        btn.id = SW_TS + '-btn';
        btn.innerHTML = '<span style="font-size:14px;line-height:1">&#9889;</span><span>日志增强模式</span>';
        Object.assign(btn.style, {
            position: 'fixed', bottom: '72px', right: '24px', zIndex: '99999',
            display: 'flex', alignItems: 'center', gap: '7px',
            padding: '10px 18px 10px 14px',
            background: 'linear-gradient(135deg, rgba(255,59,48,0.92), rgba(255,149,0,0.92))',
            backdropFilter: 'blur(20px) saturate(180%)',
            WebkitBackdropFilter: 'blur(20px) saturate(180%)',
            color: '#fff', border: '1px solid rgba(255,255,255,0.35)',
            borderRadius: '999px', cursor: 'grab', fontSize: '13px', fontWeight: '600',
            letterSpacing: '0.02em', userSelect: 'none', touchAction: 'none',
            boxShadow: '0 6px 20px rgba(255,59,48,0.32), inset 0 1px 0 rgba(255,255,255,0.25)',
            transition: 'box-shadow 0.2s ease'
        });
        // 恢复上次拖动位置（存右/下距离，窗口变化也贴边不丢）
        try {
            var saved = JSON.parse(localStorage.getItem(BTN_POS_KEY) || 'null');
            if (saved && isFinite(saved.right) && isFinite(saved.bottom)) {
                btn.style.right = Math.max(8, Math.min(saved.right, window.innerWidth - 180)) + 'px';
                btn.style.bottom = Math.max(8, Math.min(saved.bottom, window.innerHeight - 60)) + 'px';
            }
        } catch (e) { /* 忽略损坏的存储 */ }
        var dragging = false, moved = false, startX = 0, startY = 0, startRight = 0, startBottom = 0;
        btn.addEventListener('pointerdown', function (e) {
            if (e.button !== 0) return;
            dragging = true;
            var rect = btn.getBoundingClientRect();
            startX = e.clientX; startY = e.clientY;
            startRight = window.innerWidth - rect.right;
            startBottom = window.innerHeight - rect.bottom;
            btn.style.cursor = 'grabbing';
            btn.style.transition = 'none';
            try { btn.setPointerCapture(e.pointerId); } catch (err) { /* ignore */ }
        });
        btn.addEventListener('pointermove', function (e) {
            if (!dragging) return;
            var dx = e.clientX - startX, dy = e.clientY - startY;
            if (!moved && Math.abs(dx) < 5 && Math.abs(dy) < 5) return;
            moved = true;
            var rect = btn.getBoundingClientRect();
            btn.style.right = Math.max(8, Math.min(startRight - dx, window.innerWidth - rect.width - 8)) + 'px';
            btn.style.bottom = Math.max(8, Math.min(startBottom - dy, window.innerHeight - rect.height - 8)) + 'px';
        });
        function endDrag() {
            if (!dragging) return;
            dragging = false;
            btn.style.cursor = 'grab';
            btn.style.transition = '';
            if (moved) {
                try {
                    var rect = btn.getBoundingClientRect();
                    localStorage.setItem(BTN_POS_KEY, JSON.stringify({
                        right: window.innerWidth - rect.right,
                        bottom: window.innerHeight - rect.bottom
                    }));
                } catch (err) { /* ignore */ }
            }
        }
        btn.addEventListener('pointerup', endDrag);
        btn.addEventListener('pointercancel', endDrag);
        btn.addEventListener('click', function (e) {
            if (moved) { e.preventDefault(); e.stopPropagation(); moved = false; return; }
            openPlugin();
        });
        if (document.body) document.body.appendChild(btn);
    }
    function showButton() { var b = document.getElementById(SW_TS + '-btn'); if (b) b.style.display = 'flex'; }
    function hideButton() { var b = document.getElementById(SW_TS + '-btn'); if (b) b.style.display = 'none'; }

    // ==================== Resource Contention Diagnosis UI ====================
    function ensureDiagnosisModal() {
        var existing = document.getElementById(SW_TS + '-diag-modal');
        if (existing) return existing;
        var modal = document.createElement('div');
        modal.id = SW_TS + '-diag-modal';
        modal.innerHTML = '<div id="' + SW_TS + '-diag-panel">' +
            '<button class="' + SW_TS + '-diag-close" data-act="diag-close" title="关闭">&#10005;</button>' +
            '<div class="' + SW_TS + '-diag-head">' +
                '<div class="' + SW_TS + '-diag-title">进程内资源争抢诊断</div>' +
                '<div class="' + SW_TS + '-diag-sub">基于端点时序与 JVM 实例资源指标的相关性诊断，不将相关性表述为已证明的因果。</div>' +
            '</div>' +
            '<div class="' + SW_TS + '-diag-content"></div>' +
        '</div>';
        modal.addEventListener('click', function (event) {
            if (event.target === modal || event.target.closest('[data-act="diag-close"]')) {
                closeDiagnosisModal();
                return;
            }
            if (event.target.closest('[data-act="diag-submit"]')) {
                var serviceInput = modal.querySelector('[data-diag-k="service"]');
                var startInput = modal.querySelector('[data-diag-k="start"]');
                var endInput = modal.querySelector('[data-diag-k="end"]');
                startDiagnosis({
                    serviceName: serviceInput ? serviceInput.value.trim() : '',
                    serviceId: serviceInput ? serviceMap[serviceInput.value.trim()] || '' : '',
                    startTime: startInput ? startInput.value : '',
                    endTime: endInput ? endInput.value : '',
                    source: 'picker'
                });
                return;
            }
            if (event.target.closest('[data-act="diag-retry"]')) {
                startDiagnosis(modal._lastContext || getDiagnosisContext());
            }
        });
        document.body.appendChild(modal);
        return modal;
    }
    function setDiagnosisButtonLoading(loading) {
        $$('.' + SW_TS + '-diag-trigger').forEach(function (button) {
            button.disabled = loading;
            button.innerHTML = loading
                ? '<span style="animation:' + SW_TS + '-spin .8s linear infinite;display:inline-block">&#9696;</span> 正在穿透链路...'
                : '&#128269; 诊断雪崩';
        });
    }
    async function showDiagnosisContextPicker(context) {
        var generation = ++diagnosisGeneration;
        var modal = ensureDiagnosisModal();
        var content = modal.querySelector('.' + SW_TS + '-diag-content');
        modal.style.display = 'flex';
        content.innerHTML = '<div class="' + SW_TS + '-diag-loading"><div class="' + SW_TS + '-diag-spinner"></div><div>正在加载服务列表...</div></div>';
        if (!servicesReady) await loadServices();
        if (generation !== diagnosisGeneration) return;
        var names = Object.keys(serviceMap).sort();
        var options = names.slice(0, 500).map(function (name) {
            return '<option value="' + esc(name) + '"></option>';
        }).join('');
        content.innerHTML =
            '<div class="' + SW_TS + '-diag-card">' +
                '<h3>选择诊断上下文</h3>' +
                '<div class="' + SW_TS + '-diag-form">' +
                    '<label>服务<input data-diag-k="service" list="' + SW_TS + '-diag-services" value="' + esc(context && context.serviceName || '') + '" placeholder="输入服务名"></label>' +
                    '<label>开始时间<input data-diag-k="start" type="datetime-local" step="60" value="' + esc(context && context.startTime || getLocalDatetime(-30)) + '"></label>' +
                    '<label>结束时间<input data-diag-k="end" type="datetime-local" step="60" value="' + esc(context && context.endTime || getLocalDatetime(0)) + '"></label>' +
                    '<button class="' + SW_TS + '-diag-trigger" data-act="diag-submit">开始诊断</button>' +
                '</div>' +
                '<datalist id="' + SW_TS + '-diag-services">' + options + '</datalist>' +
            '</div>';
    }
    function showDiagnosisAlgorithmInfo() {
        var modal = ensureDiagnosisModal();
        var content = modal.querySelector('.' + SW_TS + '-diag-content');
        content.innerHTML = renderDiagnosisAlgorithmExplanation();
        modal.style.display = 'flex';
    }
    function formatDiagnosisDuration(value) {
        value = Number(value) || 0;
        if (value >= 1000) return (value / 1000).toFixed(value >= 10000 ? 1 : 2) + 's';
        return Math.round(value) + 'ms';
    }
    function confidenceLabel(confidence) {
        return confidence === 'high' ? '高置信' : (confidence === 'medium' ? '中置信' : '低置信');
    }
    function renderDiagnosisResult(data, diagnosis) {
        var modal = ensureDiagnosisModal();
        modal._lastContext = {
            serviceName: data.context.serviceName,
            serviceId: data.context.serviceId,
            startTime: data.context.startTime,
            endTime: data.context.endTime,
            source: data.context.source
        };
        var content = modal.querySelector('.' + SW_TS + '-diag-content');
        var suspectsHtml = diagnosis.suspects.slice(0, 5).map(function (suspect) {
            var ratio = suspect.baselineAvailable && isFinite(suspect.anomalyRatio)
                ? suspect.anomalyRatio.toFixed(1) + '× 基线'
                : '基线不可用';
            return '<div class="' + SW_TS + '-diag-item">' +
                '<div class="' + SW_TS + '-diag-name">' + esc(suspect.name) + '</div>' +
                '<div class="' + SW_TS + '-diag-meta">' +
                    '<span class="' + SW_TS + '-diag-pill ' + SW_TS + '-diag-' + suspect.confidence + '">' + confidenceLabel(suspect.confidence) + ' · ' + suspect.score + '分</span>' +
                    '<span class="' + SW_TS + '-diag-pill ' + SW_TS + '-diag-low">' + esc(suspect.type) + '</span><br>' +
                    '峰值延迟 ' + formatDiagnosisDuration(suspect.peakLatency) +
                    ' · ' + ratio +
                    ' · 峰值估算并发 ' + suspect.peakConcurrency.toFixed(2) +
                    ' · 饱和同桶候选占比 ' + (suspect.concurrencyShare * 100).toFixed(1) + '%<br>' +
                    esc(suspect.evidence.join('；') || '证据不足，仅作异常候选') +
                '</div>' +
            '</div>';
        }).join('');
        if (!suspectsHtml) {
            suspectsHtml = '<div class="' + SW_TS + '-diag-meta">没有满足嫌疑规则的端点，不强行指定“真凶”。</div>';
        }
        var sortedResources = diagnosis.resources.slice().sort(function (a, b) {
            return (b.peakRatio || 0) - (a.peakRatio || 0);
        });
        var resourcesHtml = sortedResources.slice(0, 12).map(function (resource) {
            var type = resource.resourceType === 'thread' ? '线程池' : '连接池';
            var status = resource.capacityKnown
                ? Math.round(resource.peakRatio * 100) + '% (' + Math.round(resource.peakActive) + '/' + Math.round(resource.peakMax) + ')'
                : '峰值 active ' + Math.round(resource.peakActive) + '，容量未知';
            var pill = resource.capacityKnown && resource.peakRatio >= 0.85 ? 'diag-high' : (resource.capacityKnown ? 'diag-ok' : 'diag-medium');
            return '<div class="' + SW_TS + '-diag-item">' +
                '<div class="' + SW_TS + '-diag-name">' + esc(resource.instanceName) + ' · ' + esc(resource.poolName) + '</div>' +
                '<div class="' + SW_TS + '-diag-meta"><span class="' + SW_TS + '-diag-pill ' + SW_TS + '-' + pill + '">' + type + '</span>' +
                    status + (resource.capacityKnown ? ' · 连续饱和 ' + resource.maxConsecutiveSaturated + ' 桶' : '') +
                '</div>' +
            '</div>';
        }).join('');
        if (!resourcesHtml) {
            resourcesHtml = '<div class="' + SW_TS + '-diag-meta">没有可用的线程池/连接池指标。缺数不会被解释为 0%。</div>';
        }
        var victimsHtml = diagnosis.victims.map(function (victim) {
            return '<div class="' + SW_TS + '-diag-item">' +
                '<div class="' + SW_TS + '-diag-name">' + esc(victim.name) + '</div>' +
                '<div class="' + SW_TS + '-diag-meta">资源饱和后变慢 · 峰值 ' + formatDiagnosisDuration(victim.peakLatency) +
                    ' · 估算占用贡献 ' + (victim.concurrencyShare * 100).toFixed(1) + '%</div>' +
            '</div>';
        }).join('');
        if (!victimsHtml) victimsHtml = '<div class="' + SW_TS + '-diag-meta">未识别到符合证据规则的受影响 API。</div>';
        var conclusion = diagnosis.contentionConfirmed
            ? '已发现持续资源饱和，以下为相关性最高的嫌疑端点'
            : '未确认资源争抢；展示可获得的异常证据';
        var warnings = data.warnings.length
            ? '<div class="' + SW_TS + '-diag-warn"><b>证据缺口：</b>' + esc(data.warnings.join('；')) + '</div>'
            : '';
        content.innerHTML =
            '<div class="' + SW_TS + '-diag-sub" style="margin:0 0 13px">' +
                '<b style="color:#fff">' + esc(data.context.serviceName) + '</b> · ' +
                esc(conclusion) + ' · 实例数 ' + data.instanceCount +
            '</div>' +
            '<div class="' + SW_TS + '-diag-grid">' +
                '<section class="' + SW_TS + '-diag-card"><h3>&#128308; 高风险嫌疑端点</h3>' + suspectsHtml + '</section>' +
                '<section class="' + SW_TS + '-diag-card"><h3>&#128993; 本地公共资源</h3>' + resourcesHtml + '</section>' +
                '<section class="' + SW_TS + '-diag-card"><h3>&#128994; 受影响 API</h3>' + victimsHtml + '</section>' +
            '</div>' + warnings;
    }
    function renderDiagnosisError(error, context) {
        var modal = ensureDiagnosisModal();
        modal._lastContext = context;
        var content = modal.querySelector('.' + SW_TS + '-diag-content');
        content.innerHTML =
            '<div class="' + SW_TS + '-diag-card" style="text-align:center;padding:38px">' +
                '<div style="font-size:28px;margin-bottom:10px">&#9888;</div>' +
                '<div style="font-weight:800;margin-bottom:7px">诊断未完成</div>' +
                '<div class="' + SW_TS + '-diag-meta">' + esc(error && error.message || String(error)) + '</div>' +
                '<button class="' + SW_TS + '-diag-trigger" data-act="diag-retry" style="margin:16px auto 0">重试</button>' +
            '</div>';
    }
    async function startDiagnosis(rawContext) {
        if (diagnosisRunning) return;
        rawContext = rawContext || getDiagnosisContext();
        if (!rawContext.serviceName) {
            await showDiagnosisContextPicker(rawContext);
            return;
        }
        var modal = ensureDiagnosisModal();
        var content = modal.querySelector('.' + SW_TS + '-diag-content');
        modal.style.display = 'flex';
        content.innerHTML = '<div class="' + SW_TS + '-diag-loading"><div class="' + SW_TS + '-diag-spinner"></div><div>正在穿透端点时序与实例资源指标...</div></div>';
        diagnosisRunning = true;
        setDiagnosisButtonLoading(true);
        diagnosisController = typeof AbortController !== 'undefined' ? new AbortController() : null;
        var controller = diagnosisController;
        try {
            var data = await collectDiagnosisData(rawContext, controller && controller.signal);
            var diagnosis = diagnoseContention({
                incidentStartKey: data.context.duration.incidentStartKey,
                resources: data.resources,
                candidates: data.candidates,
                instanceCount: data.instanceCount
            });
            if (controller !== diagnosisController) return;
            renderDiagnosisResult(data, diagnosis);
        } catch (error) {
            if (controller === diagnosisController && !(error && error.name === 'AbortError')) {
                renderDiagnosisError(error, rawContext);
            }
        } finally {
            if (controller === diagnosisController) {
                diagnosisRunning = false;
                diagnosisController = null;
                setDiagnosisButtonLoading(false);
            }
        }
    }
    function closeDiagnosisModal() {
        diagnosisGeneration++;
        if (diagnosisController) {
            diagnosisController.abort();
            diagnosisController = null;
        }
        diagnosisRunning = false;
        setDiagnosisButtonLoading(false);
        var modal = document.getElementById(SW_TS + '-diag-modal');
        if (modal) modal.style.display = 'none';
    }

    // ==================== UI Build ====================
    function buildUI() {
        var div = document.createElement('div');
        div.id = SW_TS + '-wrap';
        div.innerHTML =
            '<div class="' + SW_TS + '-header">' +
                '<div class="' + SW_TS + '-hl">' +
                    '<div class="' + SW_TS + '-logo">S</div>' +
                    '<div class="' + SW_TS + '-title">SkyWalking 日志增强 <span>v7.0.5 · powered by jingzhou.zhao</span></div>' +
                '</div>' +
                '<div class="' + SW_TS + '-hr">' +
                    '<button class="' + SW_TS + '-theme-btn" id="' + SW_TS + '-theme-btn" title="切换深色/浅色主题"></button>' +
                    '<button class="' + SW_TS + '-exit" data-act="exit">&#10005; 退出</button>' +
                '</div>' +
            '</div>' +
            '<div id="' + SW_TS + '-body">' +
                '<div class="' + SW_TS + '-subnav" id="' + SW_TS + '-subnav"></div>' +
                '<div class="' + SW_TS + '-qcard" id="' + SW_TS + '-qcard"></div>' +
                '<div class="' + SW_TS + '-stats" id="' + SW_TS + '-stats">' +
                    '<span>已加载 <b id="' + SW_TS + '-loaded">0</b> 条</span>' +
                    '<span>当前页 <b id="' + SW_TS + '-pages">0</b></span>' +
                    '<span id="' + SW_TS + '-state" class="warn">就绪</span>' +
                '</div>' +
                '<div class="' + SW_TS + '-list" id="' + SW_TS + '-list">' +
                    '<div class="' + SW_TS + '-loadbar" id="' + SW_TS + '-loadbar"></div>' +
                    // Per-tab content divs 由 addNewTab 动态创建，切换 tab 只切 display 不重渲染
                '</div>' +
            '</div>';
        return div;
    }

    // ==================== Render: Sub Tabs ====================
    function renderSubTabs() {
        var nav = document.getElementById(SW_TS + '-subnav');
        if (!nav) return;
        var html = '';
        tabs.forEach(function (tab, idx) {
            var active = idx === activeTabIndex ? ' active' : '';
            html += '<div class="' + SW_TS + '-sub-btn' + active + '" data-tabid="' + tab.id + '" draggable="true" title="拖拽可调整顺序，双击重命名">' +
                '<span class="tab-label" data-tabid="' + tab.id + '" title="双击重命名">' + esc(tab.name) + '</span>' +
                '<span class="cs" data-close="' + tab.id + '">&times;</span>' +
            '</div>';
        });
        html += '<button class="' + SW_TS + '-add-btn" data-act="newtab">+ 新建</button>';
        if (tabs.length > 1) {
            html += '<button class="' + SW_TS + '-close-all-btn" data-act="closeall">关闭全部</button>';
        }
        nav.innerHTML = html;
    }

    // ==================== Include Keyword Shortcuts (manual save, per service) ====================
    var IKW_SHORTCUTS_KEY = 'sw_ikw_shortcuts_v4';
    var IKW_SHORTCUTS_KEY_V3 = 'sw_ikw_shortcuts_v3';
    var IKW_SHORTCUTS_KEY_V2 = 'sw_ikw_shortcuts_v2';
    var MAX_IKW_SHORTCUTS = 30;
    var ikwDropdownScrollListener = null;

    function unbindIkwDropdownScrollClose() {
        if (ikwDropdownScrollListener) {
            window.removeEventListener('scroll', ikwDropdownScrollListener, true);
            ikwDropdownScrollListener = null;
        }
    }

    function getIkwServiceKey(tab) {
        tab = tab || tabs[activeTabIndex];
        return tab ? (tab.serviceInput || '').trim() : '';
    }

    function truncateIkwText(s, max) {
        max = max || 24;
        s = String(s || '');
        return s.length > max ? s.slice(0, max - 1) + '…' : s;
    }

    function normalizeIkwKeyword(item) {
        if (typeof item === 'string') return item.trim();
        if (item && typeof item === 'object') return String(item.keyword || item.kw || '').trim();
        return '';
    }

    function migrateIkwShortcutsLegacy() {
        var out = {};
        var tryKeys = [IKW_SHORTCUTS_KEY_V3, IKW_SHORTCUTS_KEY_V2];
        for (var k = 0; k < tryKeys.length; k++) {
            try {
                var raw = localStorage.getItem(tryKeys[k]);
                if (!raw) continue;
                var obj = JSON.parse(raw);
                if (!obj || typeof obj !== 'object' || Array.isArray(obj)) continue;
                for (var svc in obj) {
                    if (!Object.prototype.hasOwnProperty.call(obj, svc)) continue;
                    var arr = obj[svc];
                    if (!Array.isArray(arr)) continue;
                    if (!out[svc]) out[svc] = [];
                    for (var i = 0; i < arr.length; i++) {
                        var kw = normalizeIkwKeyword(arr[i]);
                        if (kw && out[svc].indexOf(kw) < 0) out[svc].push(kw);
                    }
                }
            } catch (e) { /* 静默 */ }
        }
        return Object.keys(out).length ? out : null;
    }

    function loadIkwShortcutsStore() {
        try {
            var raw = localStorage.getItem(IKW_SHORTCUTS_KEY);
            if (!raw) {
                var migrated = migrateIkwShortcutsLegacy();
                if (migrated) {
                    saveIkwShortcutsStore(migrated);
                    return migrated;
                }
                return {};
            }
            var obj = JSON.parse(raw);
            return (obj && typeof obj === 'object' && !Array.isArray(obj)) ? obj : {};
        } catch (e) { return {}; }
    }

    function saveIkwShortcutsStore(store) {
        try {
            localStorage.setItem(IKW_SHORTCUTS_KEY, JSON.stringify(store));
        } catch (e) { /* 静默 */ }
    }

    function loadIkwShortcuts(serviceKey) {
        if (!serviceKey) return [];
        try {
            var list = loadIkwShortcutsStore()[serviceKey];
            if (!Array.isArray(list)) return [];
            var out = [];
            for (var i = 0; i < list.length; i++) {
                var kw = normalizeIkwKeyword(list[i]);
                if (kw && out.indexOf(kw) < 0) out.push(kw);
            }
            return out;
        } catch (e) { return []; }
    }

    function saveIkwShortcuts(serviceKey, list) {
        if (!serviceKey) return;
        var store = loadIkwShortcutsStore();
        if (!list.length) delete store[serviceKey];
        else store[serviceKey] = list.slice(0, MAX_IKW_SHORTCUTS);
        saveIkwShortcutsStore(store);
    }

    function addIkwShortcut(serviceKey, keyword) {
        serviceKey = (serviceKey || '').trim();
        keyword = (keyword || '').trim();
        if (!serviceKey || !keyword) return false;
        var list = loadIkwShortcuts(serviceKey).filter(function (s) { return s !== keyword; });
        list.unshift(keyword);
        saveIkwShortcuts(serviceKey, list);
        return true;
    }

    function removeIkwShortcut(serviceKey, keyword) {
        serviceKey = (serviceKey || '').trim();
        keyword = (keyword || '').trim();
        if (!serviceKey || !keyword) return;
        saveIkwShortcuts(serviceKey, loadIkwShortcuts(serviceKey).filter(function (s) { return s !== keyword; }));
    }

    function fillIncludeKeyword(value) {
        var tab = tabs[activeTabIndex];
        if (!tab) return;
        tab.includeKeyword = value;
        var inp = $('#' + SW_TS + '-qcard [data-k="ikw"]');
        if (inp) inp.value = value;
        debounceSave();
    }

    function eventClickEl(e) {
        var el = e && e.target;
        if (!el) return null;
        if (el.nodeType === 3) el = el.parentElement;
        return (el && el.closest) ? el : null;
    }

    function handleIkwMenuClick(e) {
        var el = eventClickEl(e);
        if (!el) return false;
        var del = el.closest('[data-ikw-del]');
        if (del) {
            e.preventDefault();
            e.stopPropagation();
            var delTab = tabs[activeTabIndex];
            var delSvc = getIkwServiceKey(delTab);
            var delKw = del.getAttribute('data-ikw-del') || '';
            if (delSvc && delKw) {
                removeIkwShortcut(delSvc, delKw);
                refreshIkwPickRow(delTab);
                toast('已删除快捷关键词');
            }
            return true;
        }
        var fill = el.closest('[data-ikw-fill]');
        if (fill) {
            e.preventDefault();
            e.stopPropagation();
            fillIncludeKeyword(fill.getAttribute('data-ikw-fill') || '');
            closeIkwDropdown();
            toast('已填入关键词');
            var fillTab = tabs[activeTabIndex];
            if (fillTab && fillTab.autoQuery && !fillTab.loading) queryLogs(1, fillTab);
            return true;
        }
        return false;
    }

    function bindIkwMenuEvents(menu) {
        if (!menu || menu._ikwClickBound) return;
        menu._ikwClickBound = true;
        menu.addEventListener('click', handleIkwMenuClick);
    }

    function closeIkwDropdown() {
        var menu = document.querySelector('.' + SW_TS + '-ikw-dd-menu.open');
        if (!menu) {
            unbindIkwDropdownScrollClose();
            return;
        }
        menu.classList.remove('open');
        menu.style.position = '';
        menu.style.top = '';
        menu.style.left = '';
        menu.style.right = '';
        menu.style.zIndex = '';
        menu.style.maxHeight = '';
        menu.style.display = '';
        var dd = document.querySelector('.' + SW_TS + '-ikw-dd');
        if (dd && menu.parentNode !== dd) dd.appendChild(menu);
        unbindIkwDropdownScrollClose();
    }

    function positionIkwDropdown() {
        var btn = document.querySelector('[data-act="ikw-dd-toggle"]');
        var menu = document.querySelector('.' + SW_TS + '-ikw-dd-menu.open');
        if (!btn || !menu) return;
        if (menu.parentNode !== document.body) document.body.appendChild(menu);
        menu.style.position = 'fixed';
        menu.style.zIndex = '100010';
        menu.style.display = 'block';
        var rect = btn.getBoundingClientRect();
        var menuW = menu.offsetWidth || 300;
        var left = Math.max(8, Math.min(rect.right - menuW, window.innerWidth - menuW - 8));
        menu.style.top = (rect.bottom + 4) + 'px';
        menu.style.left = left + 'px';
        menu.style.right = 'auto';
        var maxH = Math.min(280, window.innerHeight - rect.bottom - 16);
        menu.style.maxHeight = Math.max(120, maxH) + 'px';
    }

    function openIkwDropdown() {
        var menu = document.querySelector('.' + SW_TS + '-ikw-dd-menu');
        if (!menu) return;
        bindIkwMenuEvents(menu);
        menu.classList.add('open');
        positionIkwDropdown();
        if (!ikwDropdownScrollListener) {
            ikwDropdownScrollListener = function (ev) {
                var openMenu = document.querySelector('.' + SW_TS + '-ikw-dd-menu.open');
                if (!openMenu) return;
                var t = ev.target;
                // 菜单内部滚动（含滚轮）不收起
                if (t === openMenu || (t && t.nodeType === 1 && openMenu.contains(t))) return;
                closeIkwDropdown();
            };
            window.addEventListener('scroll', ikwDropdownScrollListener, true);
        }
    }

    function toggleIkwDropdown() {
        var menu = document.querySelector('.' + SW_TS + '-ikw-dd-menu');
        if (!menu) return;
        if (menu.classList.contains('open')) closeIkwDropdown();
        else openIkwDropdown();
    }

    function renderIkwPickInnerHtml(tab) {
        tab = tab || tabs[activeTabIndex];
        var serviceKey = getIkwServiceKey(tab);
        if (!serviceKey) return '';
        var list = loadIkwShortcuts(serviceKey);
        var btnLabel = list.length ? ('快捷(' + list.length + ')') : '快捷';
        var menuHtml = '';
        if (list.length) {
            menuHtml = '<div class="' + SW_TS + '-ikw-dd-menu">' + list.map(function (kw) {
                return '<div class="' + SW_TS + '-ikw-dd-item" data-ikw-fill="' + esc(kw) + '" title="' + esc(kw) + '">' +
                    '<span class="' + SW_TS + '-ikw-dd-text">' + esc(truncateIkwText(kw, 42)) + '</span>' +
                    '<button type="button" class="' + SW_TS + '-ikw-dd-del" data-ikw-del="' + esc(kw) + '" title="删除此快捷">&times;</button>' +
                '</div>';
            }).join('') + '</div>';
        }
        return '<div class="' + SW_TS + '-ikw-dd">' +
            '<button type="button" class="' + SW_TS + '-ikw-dd-btn" data-act="ikw-dd-toggle" ' + (list.length ? '' : 'disabled') + ' title="快捷关键词列表">' + esc(btnLabel) + '</button>' +
            menuHtml +
        '</div>';
    }

    function renderIkwPickWrapHtml(tab) {
        var inner = renderIkwPickInnerHtml(tab);
        if (!inner) return '';
        return '<div class="' + SW_TS + '-ikw-pick-wrap" id="' + SW_TS + '-ikw-pick-wrap">' + inner + '</div>';
    }

    function refreshIkwPickRow(tab) {
        closeIkwDropdown();
        tab = tab || tabs[activeTabIndex];
        var wrap = document.getElementById(SW_TS + '-ikw-pick-wrap');
        var inner = renderIkwPickInnerHtml(tab);
        if (wrap) {
            if (inner) {
                wrap.innerHTML = inner;
                bindIkwMenuEvents(wrap.querySelector('.' + SW_TS + '-ikw-dd-menu'));
            } else wrap.remove();
        } else {
            var row = document.querySelector('#' + SW_TS + '-qcard [data-ikw-input-row]');
            if (row && inner) {
                row.insertAdjacentHTML('beforeend', '<div class="' + SW_TS + '-ikw-pick-wrap" id="' + SW_TS + '-ikw-pick-wrap">' + inner + '</div>');
                bindIkwMenuEvents(document.querySelector('.' + SW_TS + '-ikw-dd-menu'));
            }
        }
    }

    function pointInRect(x, y, rect) {
        return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    }

    function isClickInsideOpenIkwMenu(e) {
        var menu = document.querySelector('.' + SW_TS + '-ikw-dd-menu.open');
        var el = eventClickEl(e);
        if (el && (el.closest('.' + SW_TS + '-ikw-dd-menu') || el.closest('.' + SW_TS + '-ikw-dd'))) return true;
        if (menu && e.clientX != null && e.clientY != null) {
            return pointInRect(e.clientX, e.clientY, menu.getBoundingClientRect());
        }
        return false;
    }

    function handleOutsideDropdowns(e) {
        if (!isClickInsideOpenIkwMenu(e)) closeIkwDropdown();
    }

    // ==================== Render: Form (compact, no folding) ====================
    function renderActiveForm() {
        var card = document.getElementById(SW_TS + '-qcard');
        if (!card) return;
        var tab = tabs[activeTabIndex];
        if (!tab) { card.innerHTML = ''; return; }
        card.innerHTML =
            '<div class="' + SW_TS + '-qgrid">' +
                '<div class="' + SW_TS + '-fg">' +
                    '<label>服务</label>' +
                    '<div style="display:flex;gap:4px;align-items:stretch">' +
                        '<input data-k="service" list="' + SW_TS + '-service-list" placeholder="输入服务名搜索" value="' + esc(tab.serviceInput || '') + '" autocomplete="off" style="flex:1;min-width:0">' +
                        '<datalist id="' + SW_TS + '-service-list">' + renderServiceDatalistOptions(serviceMap) + '</datalist>' +
                        '<button type="button" class="' + SW_TS + '-svc-ibtn" data-act="svc-refresh" title="刷新服务列表缓存">&#x21bb;</button>' +
                    '</div>' +
                '</div>' +
                '<div class="' + SW_TS + '-fg">' +
                    '<label>包含关键词</label>' +
                    '<div data-ikw-input-row style="display:flex;gap:4px;align-items:stretch">' +
                        '<input data-k="ikw" placeholder="多个用逗号分隔" value="' + esc(tab.includeKeyword || '') + '" style="flex:1;min-width:0">' +
                        renderIkwPickWrapHtml(tab) +
                        '<button type="button" class="' + SW_TS + '-ikw-save" data-act="ikw-save" title="保存到本服务快捷列表">&#9734;</button>' +
                    '</div>' +
                '</div>' +
                '<div class="' + SW_TS + '-fg">' +
                    '<label>排除关键词</label>' +
                    '<input data-k="ekw" placeholder="多个用逗号分隔" value="' + esc(tab.excludeKeyword || '') + '">' +
                '</div>' +
                '<div class="' + SW_TS + '-fg">' +
                    '<label>级别</label>' +
                    '<select data-k="level">' +
                        '<option value="">全部</option>' +
                        ['INFO', 'WARN', 'ERROR', 'DEBUG'].map(function (lv) {
                            return '<option value="' + lv + '"' + (tab.logLevel === lv ? ' selected' : '') + '>' + lv + '</option>';
                        }).join('') +
                    '</select>' +
                '</div>' +
                '<div class="' + SW_TS + '-fg ' + SW_TS + '-q-trace">' +
                    '<label>Trace ID</label>' +
                    '<input data-k="trace" placeholder="可选" value="' + esc(tab.traceId || '') + '">' +
                '</div>' +
                '<div class="' + SW_TS + '-qtrange">' +
                    '<div class="' + SW_TS + '-fg">' +
                        '<label>开始时间</label>' +
                        '<input type="datetime-local" step="1" data-k="start" value="' + (tab.startTime || '') + '" style="min-width:0">' +
                    '</div>' +
                    '<span class="' + SW_TS + '-trange-arrow">&#8594;</span>' +
                    '<div class="' + SW_TS + '-fg">' +
                        '<label>结束时间</label>' +
                        '<input type="datetime-local" step="1" data-k="end" value="' + (tab.endTime || '') + '" style="min-width:0">' +
                    '</div>' +
                '</div>' +
                '<span class="' + SW_TS + '-qactions">' +
                    '<label class="' + SW_TS + '-autobox" title="开启后，修改任一查询条件会自动触发查询">' +
                        '<input type="checkbox" data-act="autoq" ' + (tab.autoQuery ? 'checked' : '') + '>' +
                        '<span>自动</span>' +
                    '</label>' +
                    '<button class="' + SW_TS + '-qbtn sec" data-act="reset" title="保留服务名，清空其它查询条件并恢复默认时间范围">&#x21bb; 重置</button>' +
                    '<button class="' + SW_TS + '-qbtn" data-act="search">&#128269; 查询</button>' +
                '</span>' +
            '</div>' +
            '<div class="' + SW_TS + '-frow" style="margin-top:10px">' +
                '<span class="' + SW_TS + '-qchips-label">快捷时间</span>' +
                '<span class="' + SW_TS + '-qchips">' +
                    [
                        { m: -5, t: '近5分' }, { m: -15, t: '近15分' }, { m: -30, t: '近30分' },
                        { m: -60, t: '近1时' }, { m: -360, t: '近6时' }, { m: -1440, t: '近24时' },
                        { m: -4320, t: '近3天' }, { y: 0, t: '今天' }, { y: 1, t: '昨天' }
                    ].map(function (q) {
                        if (q.y != null) {
                            return '<span class="' + SW_TS + '-qchip" data-yrange="' + (q.y === 0 ? 'today' : 'yesterday') + '">' + q.t + '</span>';
                        }
                        return '<span class="' + SW_TS + '-qchip" data-min="' + q.m + '">' + q.t + '</span>';
                    }).join('') +
                '</span>' +
                '<button class="' + SW_TS + '-diag-trigger ' + SW_TS + '-diag-compact" data-act="diagnose" type="button" title="对当前已加载日志做链路穿透，定位雪崩根因">&#128269; 诊断雪崩</button>' +
                '<button class="' + SW_TS + '-diag-info-btn" data-act="diag-info" type="button" title="查看诊断算法说明" aria-label="查看诊断算法说明">&#9432;</button>' +
            '</div>';
    }

    // ==================== Render: Log Item Card ====================
    // 隐藏未溢出的日志的 mask + 展开按钮（避免短日志也显示冗余 UI）
    function adjustItemBodies(root) {
        (root || document).querySelectorAll('.' + SW_TS + '-item-body').forEach(function (b) {
            var overflow = b.scrollHeight > b.clientHeight + 1;
            var expanded = b.classList.contains('exp');
            var mask = b.querySelector('.' + SW_TS + '-item-body-mask');
            var toggle = b.querySelector('.' + SW_TS + '-item-toggle');
            if (mask) mask.style.display = overflow ? '' : 'none';
            // 展开后无 max-height，溢出不溢出没意义；为避免「展开了却找不到收起按钮」，
            // 展开状态强制显示按钮；收起后根据实际溢出判断
            if (toggle) toggle.style.display = (overflow || expanded) ? '' : 'none';
        });
    }
    // 解析 SQL 参数数组字符串（处理嵌套引号/括号/转义）
    function parseSqlParamValues(s) {
        s = String(s || '').trim();
        if (s.charAt(0) === '[' && s.charAt(s.length - 1) === ']') s = s.slice(1, -1);
        var out = [], cur = '', inStr = false, quote = '', depth = 0;
        for (var i = 0; i < s.length; i++) {
            var ch = s.charAt(i);
            if (inStr) {
                if (ch === '\\' && i + 1 < s.length) { cur += ch + s.charAt(i + 1); i++; continue; }
                if (ch === quote) { inStr = false; cur += ch; continue; }
                cur += ch; continue;
            }
            if (ch === '"' || ch === '\'') { inStr = true; quote = ch; cur += ch; continue; }
            if (ch === '[' || ch === '{' || ch === '(') { depth++; cur += ch; continue; }
            if (ch === ']' || ch === '}' || ch === ')') { depth--; cur += ch; continue; }
            if (ch === ',' && depth === 0) { out.push(cur.trim()); cur = ''; continue; }
            cur += ch;
        }
        if (cur.trim()) out.push(cur.trim());
        return out;
    }
    // 把 ? 按顺序替换为参数值
    function substituteSqlParams(stmt, paramsStr) {
        var values = parseSqlParamValues(paramsStr);
        var idx = 0;
        return String(stmt || '').replace(/\?/g, function () {
            if (idx >= values.length) return '?';
            var v = values[idx++];
            var raw = v.replace(/^['"]|['"]$/g, '');
            if (raw === 'null') return 'NULL';
            if (/^-?\d+(\.\d+)?$/.test(raw)) return raw;
            return "'" + raw.replace(/'/g, "''") + "'";
        });
    }

    function renderLogItem(log, idx, kws) {
        // 允许外部传入 kws：避免在 await 之后的渲染循环里误读活动 tab 的关键词
        var content = log.content || '';
        var ts = new Date(log.timestamp);
        var tsStrShort = pad(ts.getMonth() + 1) + '-' + pad(ts.getDate()) +
            ' ' + pad(ts.getHours()) + ':' + pad(ts.getMinutes()) + ':' + pad(ts.getSeconds());
        var tsStrFull = ts.getFullYear() + '-' + pad(ts.getMonth() + 1) + '-' + pad(ts.getDate()) +
            ' ' + pad(ts.getHours()) + ':' + pad(ts.getMinutes()) + ':' + pad(ts.getSeconds());
        // datetime-local 格式（秒级），双击填入查询时用
        var tsStrLocal = ts.getFullYear() + '-' + pad(ts.getMonth() + 1) + '-' + pad(ts.getDate()) +
            'T' + pad(ts.getHours()) + ':' + pad(ts.getMinutes()) + ':' + pad(ts.getSeconds());

        function short(s, n) { s = String(s || ''); return s.length > n ? s.slice(0, n) + '…' : s; }

        var level = '';
        var logger = '';
        var thread = '';
        var extraHtml = '';
        (log.tags || []).forEach(function (t) {
            var k = (t.key || '').toLowerCase();
            var v = t.value || '';
            if (k === 'level') level = v;
            else if (k === 'logger') logger = v;
            else if (k === 'thread') thread = v;
            else extraHtml += '<span>' + esc(t.key) + ': ' + esc(v) + '</span>';
        });

        // 未传 kws 时回退到当前活动 tab 的关键词（switchSubTab / 初始恢复时正确）
        if (!Array.isArray(kws)) {
            var ikw = (tabs[activeTabIndex] && tabs[activeTabIndex].includeKeyword) || '';
            kws = ikw ? ikw.split(/[，,]+/).map(function (k) { return k.trim(); }).filter(Boolean) : [];
        }
        var bodyHtml = highlight(content, kws);

        var levelTag = level ? '<span class="' + SW_TS + '-item-tag lv-' + level + '" title="' + esc(level) + '">' + esc(level) + '</span>' : '';
        var svcTag = log.serviceName ? '<span class="' + SW_TS + '-item-tag svc" title="' + esc(log.serviceName) + '">' + esc(short(log.serviceName, 16)) + '</span>' : '';
        var instTag = log.serviceInstanceName ? '<span class="' + SW_TS + '-item-tag inst" title="' + esc(log.serviceInstanceName) + '">' + esc(short(log.serviceInstanceName, 16)) + '</span>' : '';
        var epTag = log.endpointName ? '<span class="' + SW_TS + '-item-tag ep" title="' + esc(log.endpointName) + '">' + esc(short(log.endpointName, 18)) + '</span>' : '';
        var loggerTag = logger ? '<span class="' + SW_TS + '-item-tag logger" title="Logger: ' + esc(logger) + '">@' + esc(short(logger, 14)) + '</span>' : '';
        var threadTag = thread ? '<span class="' + SW_TS + '-item-tag thread" title="Thread: ' + esc(thread) + '">' + esc(short(thread, 10)) + '</span>' : '';
        var traceTag = log.traceId ? '<span class="' + SW_TS + '-item-tag trace" data-copy="' + esc(log.traceId) + '" data-dblfill="' + esc(log.traceId) + '" title="点击复制 · 双击填入查询">' + esc(log.traceId) + '</span>' +
            '<span class="' + SW_TS + '-item-tag go" data-trace-go="' + esc(log.traceId) + '" title="查看调用链">&#128279;</span>' : '';

        return '' +
            '<div class="' + SW_TS + '-item lv-bg-' + (level || '') + '" data-idx="' + idx + '">' +
                '<div class="' + SW_TS + '-item-head">' +
                    '<span class="' + SW_TS + '-item-time" data-dblfill="' + esc(tsStrLocal) + '" title="' + esc(tsStrFull) + ' · 双击填入查询（开始结束都是这一秒）">' + tsStrShort + '</span>' +
                    levelTag +
                    loggerTag +
                    threadTag +
                    svcTag +
                    instTag +
                    epTag +
                    traceTag +
                '</div>' +
                '<div class="' + SW_TS + '-item-body" data-act="detail" title="双击查看日志详情">' +
                    bodyHtml +
                    '<div class="' + SW_TS + '-item-body-mask"></div>' +
                    '<button class="' + SW_TS + '-item-toggle" data-act="exp">展开</button>' +
                '</div>' +
                (extraHtml ? '<div class="' + SW_TS + '-item-extra">' + extraHtml + '</div>' : '') +
            '</div>';
    }

    function refreshStats() {
        var tab = tabs[activeTabIndex];
        var loadedEl = document.getElementById(SW_TS + '-loaded');
        var pagesEl = document.getElementById(SW_TS + '-pages');
        var stateEl = document.getElementById(SW_TS + '-state');
        if (!loadedEl || !pagesEl || !stateEl || !tab) return;
        loadedEl.textContent = tab.items ? tab.items.length : 0;
        pagesEl.textContent = tab.currentPage || 0;
        if (tab.loading) { stateEl.textContent = '查询中…'; stateEl.className = 'warn'; }
        else if (tab.hasMoreLogs === false && tab.currentPage > 0) { stateEl.textContent = '已穷尽'; stateEl.className = 'ok'; }
        else if (tab.currentPage > 0) { stateEl.textContent = '可继续加载'; stateEl.className = ''; }
        else { stateEl.textContent = '就绪'; stateEl.className = 'warn'; }
    }

    function toggleLoading(on) {
        // 只控制当前活动 tab 的 loading 状态，不再重置其它 tab 的 loading
        var tab = tabs[activeTabIndex];
        if (tab) tab.loading = on;
        syncLoadingUI();
    }

    // ==================== Empty State (per-tab) ====================
    // 每个 tab 有自己的 contentEl，empty 元素是 contentEl 内的子节点
    // 完全 DOM 隔离：切换 tab 不会互相影响
    function setEmptyState(tab, html) {
        if (!tab) return;
        tab.emptyHtml = html || null;
        if (tabs[activeTabIndex] !== tab) return;
        if (!tab.contentEl) return;
        var emptyEl = tab.contentEl.querySelector('.' + SW_TS + '-empty');
        if (!emptyEl) return;
        if (html) {
            emptyEl.style.display = 'block';
            emptyEl.innerHTML = html;
        } else {
            emptyEl.style.display = 'none';
        }
    }

    function restoreEmptyState(tab) {
        if (!tab || !tab.contentEl) return;
        var emptyEl = tab.contentEl.querySelector('.' + SW_TS + '-empty');
        if (!emptyEl) return;
        if (tab.items && tab.items.length) {
            emptyEl.style.display = 'none';
            return;
        }
        emptyEl.style.display = 'block';
        emptyEl.innerHTML = tab.emptyHtml || ('<div class="ic">&#128203;</div><div>输入查询条件，点击「查询」开始检索</div>');
    }

    // 进度条 / 按钮 / 状态文字 → 始终跟随当前活动 tab 的 loading
    function syncLoadingUI() {
        var tab = tabs[activeTabIndex];
        var loading = !!(tab && tab.loading);
        var bar = document.getElementById(SW_TS + '-loadbar');
        if (bar) bar.classList.toggle('on', loading);
        $$('.' + SW_TS + '-qbtn').forEach(function (b) { b.disabled = loading; });
        refreshStats();
    }

    function toast(msg) {
        var t = document.createElement('div');
        t.className = SW_TS + '-toast';
        t.textContent = msg;
        document.body.appendChild(t);
        requestAnimationFrame(function () { t.classList.add('show'); });
        setTimeout(function () {
            t.classList.remove('show');
            setTimeout(function () { t.remove(); }, 250);
        }, 1500);
    }

    function copyText(text, tip) {
        if (navigator.clipboard) {
            navigator.clipboard.writeText(text).then(function () { if (tip) toast(tip); });
            return;
        }
        var ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        try { document.execCommand('copy'); if (tip) toast(tip); } catch (e) { /* 忽略 */ }
        ta.remove();
    }

    // ==================== GraphQL ====================
    async function graphqlRequest(query, variables, options) {
        options = options || {};
        var timeoutMs = options.timeoutMs || 15000;
        var timeoutController = typeof AbortController !== 'undefined' ? new AbortController() : null;
        var timeoutId = timeoutController ? setTimeout(function () { timeoutController.abort(); }, timeoutMs) : null;
        var relayAbort = null;
        if (timeoutController && options.signal) {
            relayAbort = function () { timeoutController.abort(); };
            if (options.signal.aborted) relayAbort();
            else options.signal.addEventListener('abort', relayAbort, { once: true });
        }
        var signal = timeoutController ? timeoutController.signal : options.signal;
        try {
            var res = await fetch(GRAPHQL_URL, {
                method: 'POST',
                credentials: 'include',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ query: query, variables: variables || {} }),
                signal: signal
            });
            if (!res.ok) {
                return { data: null, status: res.status, error: 'GraphQL HTTP ' + res.status };
            }
            var result = await res.json();
            var graphError = result.errors && result.errors.length
                ? result.errors.map(function (e) { return e.message; }).join('; ')
                : '';
            return { data: result.data || null, status: res.status, error: graphError };
        } catch (e) {
            var aborted = e && e.name === 'AbortError';
            return { data: null, status: 0, error: aborted ? '请求已取消或超时' : (e.message || String(e)), aborted: aborted };
        } finally {
            if (timeoutId) clearTimeout(timeoutId);
            if (relayAbort && options.signal) options.signal.removeEventListener('abort', relayAbort);
        }
    }
    async function graphql(query, variables) {
        var result = await graphqlRequest(query, variables);
        if (result.error) {
            console.warn('GraphQL error:', result.error);
        }
        return result.data || null;
    }
    async function mapWithConcurrency(items, limit, worker) {
        var results = new Array(items.length);
        var cursor = 0;
        async function run() {
            while (cursor < items.length) {
                var index = cursor++;
                try {
                    results[index] = { status: 'fulfilled', value: await worker(items[index], index) };
                } catch (error) {
                    results[index] = { status: 'rejected', reason: error };
                }
            }
        }
        var runners = [];
        for (var i = 0; i < Math.min(limit, items.length); i++) runners.push(run());
        await Promise.all(runners);
        return results;
    }
    async function getDiagnosisCapabilities(signal) {
        if (diagnosisCapabilitiesCache && Date.now() - diagnosisCapabilitiesTs < 5 * 60 * 1000) {
            return diagnosisCapabilitiesCache;
        }
        var query = 'query diagnosisMetrics($regex: String) { metrics: listMetrics(regex: $regex) { name type catalog } }';
        var result = await graphqlRequest(query, {
            regex: '^(endpoint_resp_time|endpoint_cpm|meter_thread_pool|meter_datasource)$'
        }, { signal: signal, timeoutMs: 10000 });
        var capabilities = {
            endpoint_resp_time: 'available',
            endpoint_cpm: 'available',
            meter_thread_pool: 'unknown',
            meter_datasource: 'unknown',
            discoveryError: result.error || ''
        };
        if (!result.error && result.data && Array.isArray(result.data.metrics)) {
            capabilities.endpoint_resp_time = 'missing';
            capabilities.endpoint_cpm = 'missing';
            capabilities.meter_thread_pool = 'missing';
            capabilities.meter_datasource = 'missing';
            result.data.metrics.forEach(function (metric) {
                if (metric && Object.prototype.hasOwnProperty.call(capabilities, metric.name)) {
                    capabilities[metric.name] = 'available';
                }
            });
            diagnosisCapabilitiesCache = capabilities;
            diagnosisCapabilitiesTs = Date.now();
        }
        return capabilities;
    }
    function diagnosisMetricCanQuery(capabilities, name) {
        return capabilities && capabilities[name] !== 'missing';
    }
    function diagnosisRequestError(result, fallback) {
        if (!result) return new Error(fallback || '诊断查询失败');
        if (result.status === 401 || result.status === 403) {
            return new Error('GraphQL 鉴权失败（' + result.status + '）：当前环境可能需要显式 Authorization');
        }
        return new Error(result.error || fallback || '诊断查询失败');
    }
    async function resolveDiagnosisContext(context, signal) {
        context = Object.assign({}, context || {});
        if (!context.serviceName) throw new Error('请先选择要诊断的服务');
        await waitForServices();
        context.serviceId = context.serviceId || serviceMap[context.serviceName] || '';
        if (!context.serviceId) {
            var result = await graphqlRequest(
                'query diagnosisFindService($serviceName: String!) { service: findService(serviceName: $serviceName) { id name } }',
                { serviceName: context.serviceName },
                { signal: signal, timeoutMs: 10000 }
            );
            if (result.data && result.data.service) {
                context.serviceId = result.data.service.id;
                serviceMap[result.data.service.name || context.serviceName] = context.serviceId;
            } else {
                throw diagnosisRequestError(result, '找不到服务：' + context.serviceName);
            }
        }
        context.duration = buildDiagnosisDuration(context.startTime, context.endTime);
        return context;
    }
    async function fetchDiagnosisStageOne(context, capabilities, signal) {
        if (!diagnosisMetricCanQuery(capabilities, 'endpoint_resp_time')) {
            throw new Error('OAP 未提供 endpoint_resp_time，无法筛选嫌疑端点');
        }
        var topQuery = 'query diagnosisTop($condition: TopNCondition!, $duration: Duration!) {' +
            ' records: sortMetrics(condition: $condition, duration: $duration) { name id value }' +
            '}';
        var topLatencyPromise = graphqlRequest(topQuery, {
            condition: {
                name: 'endpoint_resp_time',
                parentService: context.serviceName,
                normal: true,
                scope: 'Endpoint',
                topN: 15,
                order: 'DES'
            },
            duration: context.duration.incident
        }, { signal: signal });
        var topCpmPromise = diagnosisMetricCanQuery(capabilities, 'endpoint_cpm')
            ? graphqlRequest(topQuery, {
                condition: {
                    name: 'endpoint_cpm',
                    parentService: context.serviceName,
                    normal: true,
                    scope: 'Endpoint',
                    topN: 15,
                    order: 'DES'
                },
                duration: context.duration.incident
            }, { signal: signal })
            : Promise.resolve({ data: { records: [] }, error: 'endpoint_cpm 不可用' });
        var instancesPromise = graphqlRequest(
            'query diagnosisInstances($serviceId: ID!, $duration: Duration!) {' +
            ' instances: listInstances(serviceId: $serviceId, duration: $duration) { id name }' +
            '}',
            { serviceId: context.serviceId, duration: context.duration.incident },
            { signal: signal }
        );
        var stage = await Promise.all([topLatencyPromise, topCpmPromise, instancesPromise]);
        if (!stage[0].data) throw diagnosisRequestError(stage[0], '端点延迟 TopN 查询失败');
        if (!stage[2].data) throw diagnosisRequestError(stage[2], '服务实例查询失败');
        var byKey = {};
        var candidates = [];
        function mergeRecords(records, kind) {
            (records || []).forEach(function (record) {
                var key = record.id || record.name;
                if (!byKey[key]) {
                    byKey[key] = {
                        id: record.id || '',
                        name: record.name || '未知端点',
                        topLatency: 0,
                        topCpm: 0
                    };
                    candidates.push(byKey[key]);
                }
                byKey[key][kind] = Number(record.value) || 0;
            });
        }
        mergeRecords(stage[0].data.records, 'topLatency');
        mergeRecords(stage[1].data && stage[1].data.records, 'topCpm');
        return {
            candidates: candidates.slice(0, 25),
            instances: (stage[2].data.instances || []).slice(0, 30),
            totalInstances: (stage[2].data.instances || []).length,
            warnings: [stage[0].error, stage[1].error, stage[2].error].filter(Boolean)
        };
    }
    async function fetchDiagnosisCandidateSeries(context, candidates, capabilities, signal) {
        var query = 'query diagnosisEndpointSeries(' +
            '$latency: MetricsCondition!, $cpm: MetricsCondition!, $duration: Duration!) {' +
            ' latency: readMetricsValues(condition: $latency, duration: $duration) { values { values { id value } } }' +
            ' cpm: readMetricsValues(condition: $cpm, duration: $duration) { values { values { id value } } }' +
            '}';
        return mapWithConcurrency(candidates, 4, async function (candidate) {
            var entity = {
                serviceName: context.serviceName,
                endpointName: candidate.name,
                normal: true
            };
            var result;
            if (diagnosisMetricCanQuery(capabilities, 'endpoint_cpm')) {
                result = await graphqlRequest(query, {
                    latency: { name: 'endpoint_resp_time', entity: entity },
                    cpm: { name: 'endpoint_cpm', entity: entity },
                    duration: context.duration.combined
                }, { signal: signal });
            } else {
                result = await graphqlRequest(
                    'query diagnosisEndpointLatency($condition: MetricsCondition!, $duration: Duration!) {' +
                    ' latency: readMetricsValues(condition: $condition, duration: $duration) { values { values { id value } } }' +
                    '}',
                    {
                        condition: { name: 'endpoint_resp_time', entity: entity },
                        duration: context.duration.combined
                    },
                    { signal: signal }
                );
            }
            var fallbackId = context.duration.incident.end;
            var latencyValues = result.data && result.data.latency && result.data.latency.values
                ? result.data.latency.values.values || []
                : [];
            var cpmValues = result.data && result.data.cpm && result.data.cpm.values
                ? result.data.cpm.values.values || []
                : [];
            return {
                id: candidate.id,
                name: candidate.name,
                latency: latencyValues.length ? latencyValues : [{ id: fallbackId, value: candidate.topLatency }],
                cpm: cpmValues.length ? cpmValues : [{ id: fallbackId, value: candidate.topCpm }],
                warning: result.error || ''
            };
        });
    }
    async function fetchDiagnosisResourceSeries(context, instances, capabilities, signal) {
        if (!diagnosisMetricCanQuery(capabilities, 'meter_thread_pool') &&
            !diagnosisMetricCanQuery(capabilities, 'meter_datasource')) return [];
        return mapWithConcurrency(instances, 3, async function (instance) {
            var fields = [];
            var variableDefs = ['$entity: Entity!', '$duration: Duration!'];
            var variables = {
                entity: {
                    serviceName: context.serviceName,
                    serviceInstanceName: instance.name,
                    normal: true
                },
                duration: context.duration.combined
            };
            if (diagnosisMetricCanQuery(capabilities, 'meter_thread_pool')) {
                variableDefs.push('$threadExpression: String!');
                variables.threadExpression = 'meter_thread_pool';
                fields.push('threadPool: execExpression(expression: $threadExpression, entity: $entity, duration: $duration) {' +
                    ' type error results { metric { labels { key value } } values { id value } } }');
            }
            if (diagnosisMetricCanQuery(capabilities, 'meter_datasource')) {
                variableDefs.push('$datasourceExpression: String!');
                variables.datasourceExpression = 'meter_datasource';
                fields.push('datasource: execExpression(expression: $datasourceExpression, entity: $entity, duration: $duration) {' +
                    ' type error results { metric { labels { key value } } values { id value } } }');
            }
            var result = await graphqlRequest(
                'query diagnosisResources(' + variableDefs.join(', ') + ') { ' + fields.join(' ') + ' }',
                variables,
                { signal: signal }
            );
            var resources = [];
            if (result.data && result.data.threadPool) {
                if (result.data.threadPool.type !== 'UNKNOWN') capabilities.meter_thread_pool = 'available';
                resources = resources.concat(pairResourceSeries(
                    normalizeMqeSeries(result.data.threadPool),
                    instance.name,
                    'thread',
                    context.duration.incidentStartKey,
                    context.duration.step
                ));
            }
            if (result.data && result.data.datasource) {
                if (result.data.datasource.type !== 'UNKNOWN') capabilities.meter_datasource = 'available';
                resources = resources.concat(pairResourceSeries(
                    normalizeMqeSeries(result.data.datasource),
                    instance.name,
                    'datasource',
                    context.duration.incidentStartKey,
                    context.duration.step
                ));
            }
            return {
                instance: instance,
                resources: resources,
                warning: result.error ||
                    (result.data && result.data.threadPool && result.data.threadPool.error) ||
                    (result.data && result.data.datasource && result.data.datasource.error) || ''
            };
        });
    }
    async function collectDiagnosisData(rawContext, signal) {
        var context = await resolveDiagnosisContext(rawContext, signal);
        var capabilities = await getDiagnosisCapabilities(signal);
        var stageOne = await fetchDiagnosisStageOne(context, capabilities, signal);
        if (capabilities.discoveryError) stageOne.warnings.push('指标能力探测失败：' + capabilities.discoveryError + '；资源指标将直接试探');
        var stageTwo = await Promise.all([
            fetchDiagnosisCandidateSeries(context, stageOne.candidates, capabilities, signal),
            fetchDiagnosisResourceSeries(context, stageOne.instances, capabilities, signal)
        ]);
        var candidates = [];
        var resources = [];
        var warnings = stageOne.warnings.slice();
        stageTwo[0].forEach(function (settled) {
            if (settled.status === 'fulfilled') {
                candidates.push(settled.value);
                if (settled.value.warning) warnings.push(settled.value.warning);
            } else {
                warnings.push(settled.reason && settled.reason.message || '候选端点时序查询失败');
            }
        });
        stageTwo[1].forEach(function (settled) {
            if (settled.status === 'fulfilled') {
                resources = resources.concat(settled.value.resources);
                if (settled.value.warning) warnings.push(settled.value.warning);
            } else {
                warnings.push(settled.reason && settled.reason.message || '资源指标查询失败');
            }
        });
        if (stageOne.totalInstances > stageOne.instances.length) {
            warnings.push('实例数量超过 30，仅分析前 30 个实例');
        }
        warnings.push('端点占用占比仅在 TopN 候选集合的同一饱和时间桶内计算，不代表服务全部端点');
        if (capabilities.meter_thread_pool === 'missing') warnings.push('未发现 meter_thread_pool');
        if (capabilities.meter_datasource === 'missing') warnings.push('未发现 meter_datasource');
        if (capabilities.meter_thread_pool === 'unknown') warnings.push('meter_thread_pool 能力状态未知');
        if (capabilities.meter_datasource === 'unknown') warnings.push('meter_datasource 能力状态未知');
        return {
            context: context,
            capabilities: capabilities,
            candidates: candidates,
            resources: resources,
            warnings: Array.from(new Set(warnings.filter(Boolean))),
            instanceCount: stageOne.totalInstances
        };
    }

    // 服务列表缓存
    function applyServicesData(services) {
        serviceMap = {};
        (services || []).forEach(function (s) {
            serviceMap[s.name] = s.id;
        });
        var datalist = document.getElementById(SW_TS + '-service-list');
        if (datalist) datalist.innerHTML = renderServiceDatalistOptions(serviceMap);
    }

    function saveServicesCache(services) {
        try {
            localStorage.setItem(SERVICES_CACHE_KEY, JSON.stringify({
                ts: Date.now(),
                services: services || []
            }));
        } catch (e) { /* localStorage 可能满，忽略 */ }
    }
    function loadServicesCache() {
        try {
            var raw = localStorage.getItem(SERVICES_CACHE_KEY);
            if (!raw) return null;
            var data = JSON.parse(raw);
            if (!data || !Array.isArray(data.services)) return null;
            return data;
        } catch (e) { return null; }
    }

    // 命中缓存：直接同步填入，标志置为 ready
    function applyServicesFromCache() {
        var cached = loadServicesCache();
        if (!cached) return false;
        applyServicesData(cached.services);
        servicesCacheTs = cached.ts || 0;
        servicesReady = true;
        return true;
    }

    // 远程拉取：成功后写入缓存
    async function fetchServicesFromRemote() {
        servicesReady = false;
        servicesReadyPromise = (async function () {
            try {
                var data = await graphql(
                    'query queryServices($layer: String!) { services: listServices(layer: $layer) { id name } }',
                    { layer: 'GENERAL' }
                );
                var list = (data && data.services) || [];
                applyServicesData(list);
                saveServicesCache(list);
                servicesCacheTs = Date.now();
            } catch (e) {
                console.error('Load services failed:', e);
                // 拉取失败时，如果还没填过任何东西，尝试回退到缓存
                if (!servicesReady && !serviceMap) {
                    applyServicesFromCache();
                }
            } finally {
                servicesReady = true;
            }
        })();
        return servicesReadyPromise;
    }

    async function loadServices() {
        // 优先用本地缓存（同步可用）
        if (applyServicesFromCache()) {
            // 后台异步静默刷新（不阻塞 UI）
            fetchServicesFromRemote().then(function () {
                // 刷新完成后，标记时间戳更新（如果有需要展示的）
            });
            return;
        }
        // 无缓存 → 走远程拉取
        return fetchServicesFromRemote();
    }
    async function waitForServices() {
        if (servicesReady) return;
        if (servicesReadyPromise) await servicesReadyPromise;
    }

    // ==================== Log Query ====================
    function queryErrorHtml(msg) {
        return '<div class="ic" style="color:#ff3b30;font-size:36px">&#9888;&#65039;</div>' +
            '<div style="font-size:14px;font-weight:600;color:#1d1d1f">查询失败，请重试</div>' +
            '<div style="font-size:12px;color:#86868b;margin-top:4px">' + esc(msg) + '</div>' +
            '<button class="' + SW_TS + '-tn-retry" data-act="retry-logs" style="margin-top:12px;padding:6px 18px;border:1px solid rgba(255,59,48,0.3);background:linear-gradient(135deg,#ff3b30,#ff9500);color:#fff;border-radius:8px;font-size:12px;cursor:pointer;font-weight:600">重试</button>';
    }
    function getTabQueryFields(tab) {
        tab = tab || {};
        return {
            serviceName: String(tab.serviceInput || '').trim(),
            includeKw: String(tab.includeKeyword || '').trim(),
            excludeKw: String(tab.excludeKeyword || '').trim(),
            logLevel: String(tab.logLevel || ''),
            traceId: String(tab.traceId || '').trim(),
            startInput: String(tab.startTime || ''),
            endInput: String(tab.endTime || '')
        };
    }
    async function queryLogs(page, targetTab) {
        // 等待服务列表加载完成（解决刷新页面后立即查询提示「服务未找到」的问题）
        await waitForServices();
        // 优先使用调用方传入的 tab（避免 await 期间切 tab 后查到错 tab）
        var tab = targetTab || tabs[activeTabIndex];
        if (!tab) return;
        if (tab.loading || (!tab.hasMoreLogs && page > 1)) return;
        // 记录尝试前的页码，失败时回滚（避免滚动加载失败后跳过该页）
        var prevPage = tab.currentPage || 0;
        tab.loading = true;
        tab.currentPage = page;

        var fields = getTabQueryFields(tab);
        var serviceName = fields.serviceName;
        var includeKw = fields.includeKw;
        var excludeKw = fields.excludeKw;
        var logLevel = fields.logLevel;
        var traceId = fields.traceId;
        var startInput = fields.startInput;
        var endInput = fields.endInput;
        debounceSave();

        if (!startInput || !endInput) {
            if (page === 1) toast('请选择时间范围');
            tab.loading = false;
            return;
        }

        // 时间粒度：< 24h 用 SECOND（避免分钟级分页在分钟边界处出现同分钟新增数据导致的排序混乱）
        //         >= 24h 用 HOUR（避免秒级 bucket 过多导致查询慢）
        var durationMillis = new Date(endInput).getTime() - new Date(startInput).getTime();
        var step = (durationMillis >= 24 * 60 * 60 * 1000) ? 'HOUR' : 'SECOND';
        var serviceId = '';
        if (serviceName) {
            serviceId = serviceMap[serviceName];
            if (!serviceId) {
                toast('服务未找到: ' + serviceName);
                tab.loading = false;
                return;
            }
        }

        // 降序查询（与原生 SkyWalking 页面一致）+ 列表末尾追加：列表顶部为最新日志，触底加载更早的日志
        // 配合 SECOND 粒度，分页边界处同一秒内的新数据也能量化到秒级排序，避免分钟级 bucket 切割导致的乱序
        var condition = { paging: { pageNum: page, pageSize: 20 }, queryOrder: "DES" };
        if (traceId) {
            condition.relatedTrace = { traceId: traceId };
        } else {
            condition.queryDuration = { start: formatToSkywalkingTime(startInput, step), end: formatToSkywalkingTime(endInput, step), step: step };
            if (serviceId) condition.serviceId = serviceId;
            if (includeKw) condition.keywordsOfContent = includeKw.split(/[，,]+/).map(function (s) { return s.trim(); }).filter(Boolean);
            if (excludeKw) condition.excludingKeywordsOfContent = excludeKw.split(/[，,]+/).map(function (s) { return s.trim(); }).filter(Boolean);
            if (logLevel) condition.tags = [{ key: 'level', value: logLevel }];
        }

        toggleLoading(true);
        if (page === 1) {
            tab.items = [];
            setEmptyState(tab, null);
            // 清空该 tab contentEl 内的所有 item（每个 tab 独立 DOM）
            if (tab.contentEl) {
                $$('.' + SW_TS + '-item', tab.contentEl).forEach(function (n) { n.remove(); });
            }
        }
        try {
            var data = await graphql(
                'query queryLogs($condition: LogQueryCondition) { queryLogs(condition: $condition) { logs { serviceName serviceInstanceName endpointName traceId timestamp content tags { key value } } } }',
                { condition: condition }
            );
            if (!data) {
                var hasUnsupported = (condition.tags && condition.tags.length > 0);
                if (hasUnsupported) {
                    var fallback = { paging: condition.paging, queryOrder: condition.queryOrder };
                    if (condition.relatedTrace) fallback.relatedTrace = condition.relatedTrace;
                    if (condition.queryDuration) fallback.queryDuration = condition.queryDuration;
                    if (condition.serviceId) fallback.serviceId = condition.serviceId;
                    if (condition.keywordsOfContent) fallback.keywordsOfContent = condition.keywordsOfContent;
                    if (condition.excludingKeywordsOfContent) fallback.excludingKeywordsOfContent = condition.excludingKeywordsOfContent;
                    if (logLevel) fallback.tags = [{ key: 'level', value: logLevel }];
                    data = await graphql(
                        'query queryLogs($condition: LogQueryCondition) { queryLogs(condition: $condition) { logs { serviceName serviceInstanceName endpointName traceId timestamp content tags { key value } } } }',
                        { condition: fallback }
                    );
                }
            }
            // 仍然为空 → 503 / 网络异常 / 后端报错，显示错误视图
            if (!data) {
                if (page === 1) {
                    setEmptyState(tab, queryErrorHtml('后端返回为空（可能是 503、超时或网络异常）'));
                }
                // 失败时回滚 currentPage，并保留 hasMoreLogs=true 以允许滚动重试
                tab.currentPage = prevPage;
                tab.hasMoreLogs = true;
                tab.loading = false;
                toggleLoading(false);
                return;
            }
            var logs = (data && data.queryLogs && data.queryLogs.logs) || [];

            if (logs.length === 0) {
                if (page === 1) {
                    setEmptyState(tab, '<div class="ic">&#128683;</div><div>该时间范围/条件下未匹配到日志</div>');
                }
                tab.hasMoreLogs = false;
                tab.loading = false;
                toggleLoading(false);
                return;
            }

            var base = (tab.items || []).length;
            // 解析当前 tab 的关键词（避免 await 之后误读活动 tab 的 includeKeyword）
            var kws = (tab.includeKeyword || '').split(/[，,]+/).map(function (k) { return k.trim(); }).filter(Boolean);
            // array.join 比字符串拼接快得多（V8 对此有专门优化）
            var parts = [];
            for (var i = 0; i < logs.length; i++) {
                tab.items.push(logs[i]);
                parts.push(renderLogItem(logs[i], base + i, kws));
            }
            // 隔离守卫：tab 已被关闭 / 用户已切到其它 tab → 跳过 DOM 写入，避免污染其它 tab 的视图
            if (tabs.indexOf(tab) >= 0 && tab.contentEl) {
                // 降序查询 + 列表末尾追加：列表顶部为最新日志，往末尾追加更早的日志（与原生页面一致）
                // 写入 tab 自己的 contentEl，与其他 tab 完全隔离
                tab.contentEl.insertAdjacentHTML('beforeend', parts.join(''));
                // 调整 item-body 溢出检测：移到 rAF，避免 100+ 次同步布局查询阻塞当前帧（主因卡顿）
                requestAnimationFrame(function () { if (tabs.indexOf(tab) >= 0) adjustItemBodies(tab.contentEl); });
            }

            tab.hasMoreLogs = logs.length === 20;
            tab.loading = false;
            toggleLoading(false);
            if (page === 1) toast('已加载 ' + tab.items.length + ' 条');
            else toast('已加载第 ' + page + ' 页 (累计 ' + tab.items.length + ' 条)');
        } catch (e) {
            console.error('queryLogs failed:', e);
            // 滚动加载失败时：回滚 currentPage，保留 hasMoreLogs=true 以允许继续滚动重试
            tab.currentPage = prevPage;
            tab.hasMoreLogs = true;
            tab.loading = false;
            toggleLoading(false);
            if (page === 1) {
                setEmptyState(tab, queryErrorHtml(e.message || String(e)));
            } else {
                // 滚动加载失败：toast 提示但不阻塞继续滚动
                toast('加载第 ' + page + ' 页失败，可继续滚动重试');
            }
        }
    }

    // ==================== Tab Management ====================
    // 创建并挂载 tab 的 content 容器（每个 tab 独立 DOM）
    function createTabContentEl(tab) {
        var list = document.getElementById(SW_TS + '-list');
        if (!list) return null;
        var contentEl = document.createElement('div');
        contentEl.className = SW_TS + '-tab-content';
        contentEl.setAttribute('data-tab-id', tab.id);
        contentEl.style.display = 'none';
        // empty 元素挂在 contentEl 内部 → 每个 tab 完全独立
        var emptyEl = document.createElement('div');
        emptyEl.className = SW_TS + '-empty';
        emptyEl.innerHTML = '<div class="ic">&#128203;</div><div>输入查询条件，点击「查询」开始检索</div>';
        contentEl.appendChild(emptyEl);
        list.appendChild(contentEl);
        return contentEl;
    }

    function addNewTab(preset) {
        var id = 'tab_' + Date.now();
        var tab = {
            id: id, name: (preset && preset.name) || ('查询 ' + (tabs.length + 1)),
            serviceInput: (preset && preset.serviceInput) || '',
            includeKeyword: (preset && preset.includeKeyword) || '',
            excludeKeyword: (preset && preset.excludeKeyword) || '',
            logLevel: (preset && preset.logLevel) || '',
            traceId: (preset && preset.traceId) || '',
            startTime: (preset && preset.startTime) || getLocalDatetime(-30),
            endTime: (preset && preset.endTime) || getLocalDatetime(0),
            currentPage: 0, hasMoreLogs: true, loading: false, items: [], emptyHtml: null, scrollTop: 0
        };
        // 1. 创建 content 容器
        tab.contentEl = createTabContentEl(tab);
        // 2. 把旧 tab 的 content 全部隐藏，新 tab 的 content 显示（最后一个）
        tabs.forEach(function (t) { if (t.contentEl) t.contentEl.style.display = 'none'; });
        if (tab.contentEl) tab.contentEl.style.display = '';
        tabs.push(tab);
        activeTabIndex = tabs.length - 1;
        renderSubTabs();
        renderActiveForm();
        // 切到新 tab：重置滚动条 + 抑制 500ms 内的 scroll 事件
        var list = document.getElementById(SW_TS + '-list');
        if (list) {
            suppressScrollUntil = Date.now() + 500;
            list.scrollTop = 0;
        }
        // empty 由 contentEl 自带，restoreEmptyState 控制其内容/显隐
        restoreEmptyState(tab);
        refreshStats();
        syncLoadingUI();
        debounceSave();
    }

    function reorderTab(fromId, toId, place) {
        var from = -1, to = -1;
        tabs.forEach(function (t, i) { if (t.id === fromId) from = i; if (t.id === toId) to = i; });
        if (from < 0 || to < 0 || from === to) return;
        var activeId = (tabs[activeTabIndex] || {}).id;
        var moved = tabs.splice(from, 1)[0];
        var target = to > from ? to - 1 : to;
        tabs.splice(place === 'after' ? target + 1 : target, 0, moved);
        tabs.forEach(function (t, i) { if (t.id === activeId) activeTabIndex = i; });
        renderSubTabs();
        debounceSave();
    }

    function switchSubTab(tabId) {
        var idx = -1;
        tabs.forEach(function (t, i) { if (t.id === tabId) idx = i; });
        if (idx < 0) return;
        var list = document.getElementById(SW_TS + '-list');
        // 在改 activeTabIndex 之前保存旧 tab 的滚动位置
        if (list && activeTabIndex >= 0 && tabs[activeTabIndex]) {
            tabs[activeTabIndex].scrollTop = list.scrollTop || 0;
        }
        activeTabIndex = idx;
        renderSubTabs();
        renderActiveForm();
        if (list) {
            // 关键：先设 suppression 标志，再做 DOM 变更（display 切换可能触发 scroll 事件）
            suppressScrollUntil = Date.now() + 500;
            // 架构级隔离：只切 contentEl 的 display，不重渲染任何 items
            // 旧 tab 的 items 留在 DOM 里（被 display:none 隐藏），切回去时无需重新构建
            var newTab = tabs[activeTabIndex];
            tabs.forEach(function (t, i) {
                if (t.contentEl) t.contentEl.style.display = (i === activeTabIndex) ? '' : 'none';
            });
            // 重置滚动条到顶部（隐藏的 content 不再贡献 scrollHeight，新 content 的 maxScrollTop 可能不同）
            list.scrollTop = 0;
            // 恢复新 tab 的滚动位置（在 rAF 中，等浏览器处理完 display 切换后再设）
            if (newTab && newTab.scrollTop) {
                requestAnimationFrame(function () {
                    if (tabs[activeTabIndex] === newTab) {
                        list.scrollTop = Math.min(newTab.scrollTop, Math.max(0, list.scrollHeight - list.clientHeight));
                    }
                });
            }
        }
        refreshStats();
        syncLoadingUI();
        debounceSave();
    }

    function closeTab(tabId) {
        var idx = -1;
        tabs.forEach(function (t, i) { if (t.id === tabId) idx = i; });
        if (idx < 0) return;
        // 移除该 tab 的 content 容器（彻底清理 DOM）
        var closedTab = tabs[idx];
        if (closedTab && closedTab.contentEl && closedTab.contentEl.parentNode) {
            closedTab.contentEl.parentNode.removeChild(closedTab.contentEl);
        }
        tabs.splice(idx, 1);
        if (tabs.length === 0) { addNewTab(); return; }
        if (activeTabIndex >= tabs.length) activeTabIndex = tabs.length - 1;
        if (activeTabIndex === idx) activeTabIndex = Math.max(0, idx - 1);
        switchSubTab(tabs[activeTabIndex].id);
        debounceSave();
    }

    function closeAllTabs() {
        // 清理所有 tab 的 content 容器
        tabs.forEach(function (t) {
            if (t.contentEl && t.contentEl.parentNode) t.contentEl.parentNode.removeChild(t.contentEl);
        });
        tabs = [];
        activeTabIndex = -1;
        addNewTab();
        debounceSave();
    }

    function startEditTabName(label, tabId) {
        var tab = null;
        tabs.forEach(function (t) { if (t.id === tabId) tab = t; });
        if (!tab || label.classList.contains('editing')) return;
        var oldName = tab.name;
        var input = document.createElement('input');
        input.type = 'text';
        input.value = oldName;
        input.maxLength = 30;
        input.className = 'tab-label-input';
        input.spellcheck = false;
        label.classList.add('editing');
        label.innerHTML = '';
        label.appendChild(input);
        // draggable 祖先会吞掉输入框内的鼠标选字，编辑期间临时关掉
        var btnEl = label.closest('.' + SW_TS + '-sub-btn');
        if (btnEl) btnEl.setAttribute('draggable', 'false');
        input.focus();
        input.select();
        var finished = false;
        var done = function (commit) {
            if (finished) return;
            finished = true;
            if (btnEl) btnEl.setAttribute('draggable', 'true');
            var v = (input.value || '').trim();
            if (commit && v && v !== oldName) {
                tab.name = v.slice(0, 30);
                debounceSave();
            }
            label.classList.remove('editing');
            label.textContent = tab.name;
        };
        input.addEventListener('keydown', function (ev) {
            if (ev.key === 'Enter') { ev.preventDefault(); done(true); }
            else if (ev.key === 'Escape') { ev.preventDefault(); done(false); }
        });
        input.addEventListener('blur', function () { done(true); });
        input.addEventListener('mousedown', function (ev) { ev.stopPropagation(); });
        input.addEventListener('click', function (ev) { ev.stopPropagation(); });
        input.addEventListener('dblclick', function (ev) { ev.stopPropagation(); });
    }

    // ==================== Persistence ====================
    var TABS_STORAGE_KEY = 'sw_logs_tabs_v2';

    function saveTabsState() {
        try {
            var data = {
                activeTabId: (tabs[activeTabIndex] && tabs[activeTabIndex].id) || '',
                tabs: tabs.map(function (t) {
                    return {
                        id: t.id, name: t.name,
                        serviceInput: t.serviceInput || '',
                        includeKeyword: t.includeKeyword || '',
                        excludeKeyword: t.excludeKeyword || '',
                        logLevel: t.logLevel || '',
                        traceId: t.traceId || '',
                        startTime: t.startTime || '',
                        endTime: t.endTime || ''
                    };
                })
            };
            localStorage.setItem(TABS_STORAGE_KEY, JSON.stringify(data));
        } catch (e) { /* 静默 */ }
    }

    function loadTabsState() {
        try {
            var raw = localStorage.getItem(TABS_STORAGE_KEY);
            if (!raw) return false;
            var data = JSON.parse(raw);
            if (!data || !Array.isArray(data.tabs) || data.tabs.length === 0) return false;
            var restoredActiveId = data.activeTabId || '';
            var restoredActiveIdx = -1;
            for (var i = 0; i < data.tabs.length; i++) {
                var saved = data.tabs[i];
                var tab = {
                    id: saved.id || ('tab_' + Date.now() + '_' + i),
                    name: saved.name || ('标签页 ' + (i + 1)),
                    serviceInput: saved.serviceInput || '',
                    includeKeyword: saved.includeKeyword || '',
                    excludeKeyword: saved.excludeKeyword || '',
                    logLevel: saved.logLevel || '',
                    traceId: saved.traceId || '',
                    startTime: saved.startTime || getLocalDatetime(-30),
                    endTime: saved.endTime || getLocalDatetime(0),
                    currentPage: 0, hasMoreLogs: true, loading: false, items: [], emptyHtml: null, scrollTop: 0
                };
                // 为每个恢复的 tab 创建独立的 content 容器
                tab.contentEl = createTabContentEl(tab);
                tabs.push(tab);
                if (tab.id === restoredActiveId) restoredActiveIdx = tabs.length - 1;
            }
            activeTabIndex = restoredActiveIdx >= 0 ? restoredActiveIdx : 0;
            // 应用可见性：只有 activeTab 的 contentEl 显示
            tabs.forEach(function (t, i) {
                if (t.contentEl) t.contentEl.style.display = (i === activeTabIndex) ? '' : 'none';
            });
            renderSubTabs();
            renderActiveForm();
            return true;
        } catch (e) {
            return false;
        }
    }

    // ==================== Event Binding ====================
    function bindEvents() {
        // 捕获阶段监听：在子元素 stopPropagation 之前判断，点击其他区域自动收起下拉
        document.addEventListener('click', handleOutsideDropdowns, true);

        // 双击检测 helper
        var _lastTabClick = { tabId: null, time: 0 };
        function maybeStartEdit(label, tabId, e) {
            var now = Date.now();
            if (_lastTabClick.tabId === tabId && (now - _lastTabClick.time) < 400) {
                _lastTabClick = { tabId: null, time: 0 };
                e.preventDefault(); e.stopPropagation();
                startEditTabName(label, tabId);
                return true;
            }
            _lastTabClick = { tabId: tabId, time: now };
            return false;
        }

        // 标签栏点击
        var subnav = document.getElementById(SW_TS + '-subnav');
        subnav.addEventListener('click', function (e) {
            if (e.target.classList && e.target.classList.contains('tab-label-input')) return;
            var label = e.target.closest && e.target.closest('.tab-label');
            if (label) {
                var tabBtn = label.closest('.' + SW_TS + '-sub-btn');
                var tabId = tabBtn && tabBtn.getAttribute('data-tabid');
                if (tabId && !label.classList.contains('editing')) {
                    if (maybeStartEdit(label, tabId, e)) return;
                }
            }
            var closeBtn = e.target.closest('[data-close]');
            if (closeBtn) { e.stopPropagation(); closeTab(closeBtn.getAttribute('data-close')); return; }
            if (e.target.closest('[data-act="newtab"]')) { addNewTab(); return; }
            if (e.target.closest('[data-act="closeall"]')) { closeAllTabs(); return; }
            var tb = e.target.closest('.' + SW_TS + '-sub-btn');
            if (tb) { switchSubTab(tb.getAttribute('data-tabid')); return; }
        });

        // 标签拖拽排序：新顺序经 debounceSave → saveTabsState 持久化，跟标签状态一起带到其他域名
        var dragTabId = null;
        function subBtnEls() { return Array.prototype.slice.call(subnav.querySelectorAll('.' + SW_TS + '-sub-btn')); }
        function clearDragMarks() {
            subBtnEls().forEach(function (b) { b.classList.remove(SW_TS + '-dragging', SW_TS + '-drop-l', SW_TS + '-drop-r'); });
        }
        subnav.addEventListener('dragstart', function (e) {
            var btn = e.target.closest && e.target.closest('.' + SW_TS + '-sub-btn');
            if (!btn) return;
            dragTabId = btn.getAttribute('data-tabid');
            btn.classList.add(SW_TS + '-dragging');
            try {
                e.dataTransfer.setData('text/plain', dragTabId);
                e.dataTransfer.effectAllowed = 'move';
            } catch (err) {}
        });
        subnav.addEventListener('dragover', function (e) {
            if (!dragTabId) return;
            var btn = e.target.closest && e.target.closest('.' + SW_TS + '-sub-btn');
            var overId = btn && btn.getAttribute('data-tabid');
            if (!overId || overId === dragTabId) return;
            e.preventDefault();
            try { e.dataTransfer.dropEffect = 'move'; } catch (err) {}
            var rect = btn.getBoundingClientRect();
            var place = (e.clientX - rect.left) < rect.width / 2 ? 'l' : 'r';
            subBtnEls().forEach(function (b) { b.classList.remove(SW_TS + '-drop-l', SW_TS + '-drop-r'); });
            btn.classList.add(SW_TS + '-drop-' + place);
        });
        subnav.addEventListener('drop', function (e) {
            if (!dragTabId) return;
            e.preventDefault();
            var btn = e.target.closest && e.target.closest('.' + SW_TS + '-sub-btn');
            var mark = btn && (btn.classList.contains(SW_TS + '-drop-l') ? 'before' : btn.classList.contains(SW_TS + '-drop-r') ? 'after' : null);
            if (btn && mark) reorderTab(dragTabId, btn.getAttribute('data-tabid'), mark);
            clearDragMarks();
            dragTabId = null;
        });
        subnav.addEventListener('dragend', function () { clearDragMarks(); dragTabId = null; });

        // 退出
        document.querySelector('.' + SW_TS + '-exit').addEventListener('click', closePlugin);

        // 深色主题切换
        var themeBtn = document.getElementById(SW_TS + '-theme-btn');
        if (themeBtn) themeBtn.addEventListener('click', toggleTheme);

        // 列表区点击
        var listEl = document.getElementById(SW_TS + '-list');
        listEl.addEventListener('click', function (e) {
            // 重试查询日志
            if (e.target.closest('[data-act="retry-logs"]')) {
                e.stopPropagation();
                // 传入当前活动 tab：await 期间切 tab 也不会让重试加到错 tab
                queryLogs(1, tabs[activeTabIndex]);
                return;
            }
            // 展开/收起
            var exp = e.target.closest('[data-act="exp"]');
            if (exp) {
                e.stopPropagation();
                var body = exp.closest('.' + SW_TS + '-item-body');
                if (body) {
                    body.classList.toggle('exp');
                    var expanded = body.classList.contains('exp');
                    exp.textContent = expanded ? '收起' : '展开';
                    // 重新检测溢出（展开后无 max-height，肯定不溢出；收起后回到 140px）
                    adjustItemBodies(body.parentNode || body);
                }
                return;
            }
            // 复制 traceId
            var traceEl = e.target.closest('[data-copy]');
            if (traceEl) {
                e.stopPropagation();
                var v = traceEl.getAttribute('data-copy');
                if (v) copyText(v, '已复制 TraceID');
                return;
            }
            // 直接查看调用链（🔗 图标）
            var traceGo = e.target.closest('[data-trace-go]');
            if (traceGo) {
                e.stopPropagation();
                var item2 = traceGo.closest('.' + SW_TS + '-item');
                if (item2) {
                    var idx2 = parseInt(item2.getAttribute('data-idx'), 10);
                    var tab2 = tabs[activeTabIndex];
                    if (tab2 && tab2.items && tab2.items[idx2]) {
                        showLogModal(tab2.items[idx2].content || '', tab2.items[idx2].traceId || '');
                        // 直接切到 trace 视图
                        setTimeout(function () {
                            var modal = document.getElementById(SW_TS + '-modal');
                            var body = document.getElementById(SW_TS + '-modal-body');
                            if (!modal || !body) return;
                            var traceTab = body.querySelector('.' + SW_TS + '-mtab[data-mtab="trace"]');
                            if (traceTab) {
                                $$('.' + SW_TS + '-mtab', modal).forEach(function (b) { b.classList.toggle('active', b === traceTab); });
                                $$('.' + SW_TS + '-modal-view', body).forEach(function (v) {
                                    v.style.display = v.getAttribute('data-view') === 'trace' ? 'block' : 'none';
                                });
                                loadTraceIntoModal(tab2.items[idx2].traceId, body);
                            }
                        }, 0);
                    }
                }
                return;
            }
            // 单击：仅做选中/复制不再弹窗（弹窗改双击，避免误触打断复制）
        });

        // 列表双击 - 日志正文弹窗 / trace 标签自动填入查询条件
        listEl.addEventListener('dblclick', function (e) {
            // 双击时间标签自动填入这一秒的查询（开始=结束=该秒，闭区间 → 查询该秒内的所有日志）
            var timeEl = e.target.closest('.' + SW_TS + '-item-time');
            if (timeEl) {
                e.stopPropagation(); e.preventDefault();
                var tsLocal = timeEl.getAttribute('data-dblfill');
                if (!tsLocal) return;
                var startInput = $('#' + SW_TS + '-qcard [data-k="start"]');
                var endInput = $('#' + SW_TS + '-qcard [data-k="end"]');
                if (startInput) startInput.value = tsLocal;
                if (endInput) endInput.value = tsLocal;
                var t = tabs[activeTabIndex];
                if (t) {
                    t.startTime = tsLocal;
                    t.endTime = tsLocal;
                    debounceSave();
                }
                queryLogs(1, t);
                toast('已填入这一秒的查询：' + tsLocal);
                return;
            }
            // 双击 trace 标签自动填入查询
            var traceEl = e.target.closest('[data-dblfill]');
            if (traceEl) {
                e.stopPropagation(); e.preventDefault();
                var tid = traceEl.getAttribute('data-dblfill');
                if (!tid) return;
                var traceInput = $('#' + SW_TS + '-qcard [data-k="trace"]');
                if (traceInput) {
                    traceInput.value = tid;
                    var t = tabs[activeTabIndex];
                    if (t) {
                        t.traceId = tid;
                        debounceSave();
                    }
                    // 传入当前活动 tab：防御 await 期间切 tab
                    queryLogs(1, t);
                    toast('已填入 TraceID 并查询');
                }
                return;
            }
            // 双击正文 → 弹窗
            var bodyEl = e.target.closest('[data-act="detail"]');
            if (bodyEl) {
                e.stopPropagation(); e.preventDefault();
                var item = bodyEl.closest('.' + SW_TS + '-item');
                if (item) {
                    var idx = parseInt(item.getAttribute('data-idx'), 10);
                    var tab = tabs[activeTabIndex];
                    if (tab && tab.items && tab.items[idx]) {
                        showLogModal(tab.items[idx].content || '', tab.items[idx].traceId || '');
                    }
                }
            }
        });

        // 列表滚动加载
        listEl.addEventListener('scroll', function () {
            // 切 tab 期间抑制：避免旧 tab 的滚动位置带到新 tab 后误触触底分页
            if (Date.now() < suppressScrollUntil) return;
            var tab = tabs[activeTabIndex];
            if (!tab) return;
            // 列表为空或不够滚动 → 不触发（避免新 tab 刚创建、列表为空时立即触发一次）
            if (listEl.scrollHeight - listEl.clientHeight <= 60) return;
            if (listEl.scrollTop + listEl.clientHeight >= listEl.scrollHeight - 60 && !tab.loading && tab.hasMoreLogs) {
                // 不再自增 currentPage，由 queryLogs 内部管理；失败时回滚
                // 传入捕获的 tab：await 期间切 tab 也不会让分页加到错 tab
                queryLogs((tab.currentPage || 0) + 1, tab);
            }
        });

        // 表单点击
        var qcard = document.getElementById(SW_TS + '-qcard');
        qcard.addEventListener('click', function (e) {
            // 快捷时间（相对）
            var chip = e.target.closest('[' + 'data-min' + ']');
            if (chip) {
                var minutes = parseInt(chip.getAttribute('data-min'), 10);
                var tab = tabs[activeTabIndex];
                if (tab) {
                    tab.startTime = getLocalDatetime(minutes);
                    tab.endTime = getLocalDatetime(0);
                    var si = $('#' + SW_TS + '-qcard [data-k="start"]');
                    var ei = $('#' + SW_TS + '-qcard [data-k="end"]');
                    if (si) si.value = tab.startTime;
                    if (ei) ei.value = tab.endTime;
                    debounceSave();
                    if (!tab.loading) queryLogs(1, tab);
                }
                return;
            }
            // 快捷时间（今天 / 昨天：00:00:00 ~ 23:59:59）
            var ychip = e.target.closest('[data-yrange]');
            if (ychip) {
                var kind = ychip.getAttribute('data-yrange');
                if (kind === 'today' || kind === 'yesterday') {
                    var tab2 = tabs[activeTabIndex];
                    if (tab2) {
                        var d2 = new Date();
                        if (kind === 'yesterday') d2.setDate(d2.getDate() - 1);
                        var y2 = d2.getFullYear();
                        var mo2 = pad(d2.getMonth() + 1);
                        var da2 = pad(d2.getDate());
                        tab2.startTime = y2 + '-' + mo2 + '-' + da2 + 'T00:00';
                        tab2.endTime = y2 + '-' + mo2 + '-' + da2 + 'T23:59';
                        var si2 = $('#' + SW_TS + '-qcard [data-k="start"]');
                        var ei2 = $('#' + SW_TS + '-qcard [data-k="end"]');
                        if (si2) si2.value = tab2.startTime;
                        if (ei2) ei2.value = tab2.endTime;
                        debounceSave();
                        if (!tab2.loading) queryLogs(1, tab2);
                    }
                }
                return;
            }
            // 快捷关键词：展开下拉
            if (e.target.closest('[data-act="ikw-dd-toggle"]')) {
                toggleIkwDropdown();
                return;
            }
            // 包含关键词：手动保存
            if (e.target.closest('[data-act="ikw-save"]')) {
                var saveTab = tabs[activeTabIndex];
                var saveSvc = getIkwServiceKey(saveTab);
                if (!saveSvc) { toast('请先填写服务名，快捷关键词按服务保存'); return; }
                var ikwInp = $('#' + SW_TS + '-qcard [data-k="ikw"]');
                var ikwVal = ikwInp ? ikwInp.value.trim() : '';
                if (!ikwVal) { toast('请先输入要保存的关键词'); return; }
                addIkwShortcut(saveSvc, ikwVal);
                refreshIkwPickRow(saveTab);
                toast('已保存快捷关键词');
                return;
            }
            if (e.target.closest('[data-act="search"]')) { queryLogs(1, tabs[activeTabIndex]); return; }
            // 重置查询条件（保留服务名）
            if (e.target.closest('[data-act="diagnose"]')) {
                startDiagnosis(getDiagnosisContext());
                return;
            }
            if (e.target.closest('[data-act="diag-info"]')) {
                showDiagnosisAlgorithmInfo();
                return;
            }
            if (e.target.closest('[data-act="reset"]')) {
                var rtab = tabs[activeTabIndex];
                if (!rtab) return;
                if (rtab.loading) { toast('正在查询中，请稍候'); return; }
                rtab.includeKeyword = '';
                rtab.excludeKeyword = '';
                rtab.logLevel = '';
                rtab.traceId = '';
                rtab.startTime = getLocalDatetime(-30);
                rtab.endTime = getLocalDatetime(0);
                rtab.currentPage = 0;
                rtab.hasMoreLogs = true;
                rtab.items = [];
                rtab.scrollTop = 0;
                renderActiveForm();
                // 只清空该 tab 自己的 contentEl（per-tab 隔离）
                if (rtab.contentEl) {
                    $$('.' + SW_TS + '-item', rtab.contentEl).forEach(function (n) { n.remove(); });
                }
                var rlist = document.getElementById(SW_TS + '-list');
                if (rlist) rlist.scrollTop = 0;
                setEmptyState(rtab, null);
                refreshStats();
                debounceSave();
                toast('已重置查询条件');
                return;
            }
            // 刷新服务列表缓存
            if (e.target.closest('[data-act="svc-refresh"]')) {
                e.preventDefault();
                var btn = e.target.closest('[data-act="svc-refresh"]');
                if (btn) { btn.disabled = true; btn.style.opacity = '0.5'; btn.style.transform = 'rotate(360deg)'; btn.style.transition = 'transform 0.6s ease'; }
                fetchServicesFromRemote().then(function () {
                    if (btn) { btn.style.transform = 'rotate(0deg)'; setTimeout(function () { btn.disabled = false; btn.style.opacity = ''; btn.style.transition = ''; }, 200); }
                    toast('服务列表已刷新');
                });
                return;
            }
        });

        // 表单 input 事件 → 自动持久化 + 自动查询
        qcard.addEventListener('input', function (e) {
            var tab = tabs[activeTabIndex];
            if (!tab) return;
            var t = e.target;
            if (!t.getAttribute) return;
            var k = t.getAttribute('data-k');
            if (!k) return;
            if (k === 'service') {
                tab.serviceInput = t.value.trim();
                refreshIkwPickRow(tab);
            } else if (k === 'ikw') tab.includeKeyword = t.value.trim();
            else if (k === 'ekw') tab.excludeKeyword = t.value.trim();
            else if (k === 'level') tab.logLevel = t.value;
            else if (k === 'trace') tab.traceId = t.value.trim();
            else if (k === 'start') tab.startTime = t.value;
            else if (k === 'end') tab.endTime = t.value;
            debounceSave();
            clearTimeout(autoQueryTimer);
            // 未开启自动查询 → 不触发
            if (!tab.autoQuery) return;
            if (k === 'service' && serviceMap[tab.serviceInput] && !tab.loading) {
                queryLogs(1, tab);
                return;
            }
            // 捕获 tab：切到其它 tab 后 auto-query 仍然对原 tab 生效（数据正确，DOM 由 queryLogs 守卫）
            var pendingTab = tab;
            autoQueryTimer = setTimeout(function () {
                if (pendingTab && !pendingTab.loading) queryLogs(1, pendingTab);
            }, 1500);
        });

        // 自动查询开关
        qcard.addEventListener('change', function (e) {
            var t = e.target;
            if (!t.getAttribute) return;
            if (t.getAttribute('data-act') !== 'autoq') return;
            var tab = tabs[activeTabIndex];
            if (!tab) return;
            tab.autoQuery = !!t.checked;
            debounceSave();
            // 开启时如果当前已有查询条件，触发一次
            if (tab.autoQuery) {
                clearTimeout(autoQueryTimer);
                if (serviceMap && serviceMap[(tab.serviceInput || '').trim()] && !tab.loading) {
                    queryLogs(1, tab);
                }
            } else {
                clearTimeout(autoQueryTimer);
            }
        });

        qcard.addEventListener('keydown', function (e) {
            if (e.key !== 'Enter' || e.target.tagName !== 'INPUT') return;
            if (e.target.getAttribute('data-k') === 'service') return;
            e.preventDefault();
            // 传入当前活动 tab：防御 await 期间切 tab
            queryLogs(1, tabs[activeTabIndex]);
        });

        // 弹窗
        var modal = document.getElementById(SW_TS + '-modal');
        if (modal) {
            modal.addEventListener('click', function (e) {
                if (e.target === modal || e.target.closest('.' + SW_TS + '-modal-x')) closeModal();
                else if (e.target.closest('.' + SW_TS + '-mtab')) {
                    var tab = e.target.closest('.' + SW_TS + '-mtab');
                    var mode = tab.getAttribute('data-mtab');
                    var body = document.getElementById(SW_TS + '-modal-body');
                    if (!body) return;
                    $$('.' + SW_TS + '-mtab', modal).forEach(function (b) { b.classList.toggle('active', b === tab); });
                    $$('.' + SW_TS + '-modal-view', body).forEach(function (v) {
                        v.style.display = v.getAttribute('data-view') === mode ? 'block' : 'none';
                    });
                    // 进入 trace 视图：tab 始终保持原样（不再切全屏，避免缩小）
                    if (mode === 'trace' && body._traceId) {
                        loadTraceIntoModal(body._traceId, body);
                    }
                }
                // 重试加载调用链
                else if (e.target.closest('[data-act="retry-trace"]')) {
                    e.stopPropagation();
                    var mbody = document.getElementById(SW_TS + '-modal-body');
                    if (mbody && mbody._traceId) loadTraceIntoModal(mbody._traceId, mbody);
                }
                // 调用链中的复制 SQL
                else if (e.target.closest('[data-act="copy-sql"]')) {
                    e.stopPropagation();
                    e.preventDefault();
                    var tnCopy = e.target.closest('[data-act="copy-sql"]');
                    var sql2 = tnCopy.getAttribute('data-sql') || '';
                    if (sql2) {
                        copyText(sql2, '已复制完整 SQL');
                        var orig = tnCopy.textContent;
                        tnCopy.textContent = '✅';
                        setTimeout(function () { tnCopy.textContent = orig || '📋'; }, 1200);
                    }
                }
            });
        }
    }

    // ==================== Modal: Log Detail ====================
    function showLogModal(content, traceId) {
        var modal = document.getElementById(SW_TS + '-modal');
        var body = document.getElementById(SW_TS + '-modal-body');
        if (!modal || !body) return;
        var html = '<div class="' + SW_TS + '-modal-tabs">' +
            '<button class="' + SW_TS + '-mtab active" data-mtab="raw">原文</button>' +
            '<button class="' + SW_TS + '-mtab" data-mtab="json">JSON 格式化</button>' +
            '<button class="' + SW_TS + '-mtab" data-mtab="search">关键词高亮</button>' +
            (traceId ? '<button class="' + SW_TS + '-mtab" data-mtab="trace" style="margin-left:auto;background:rgba(255,59,48,0.1);color:#ff3b30;border:1px solid rgba(255,59,48,0.25)">&#128279; 查看调用链</button>' : '') +
            '</div>';
        html += '<div class="' + SW_TS + '-modal-view" data-view="raw"><pre class="' + SW_TS + '-modal-pre">' + esc(content) + '</pre></div>';
        html += '<div class="' + SW_TS + '-modal-view" data-view="json" style="display:none"><pre class="' + SW_TS + '-modal-pre"></pre></div>';
        html += '<div class="' + SW_TS + '-modal-view" data-view="search" style="display:none"><input class="' + SW_TS + '-modal-search" placeholder="输入关键词,回车高亮"><div class="' + SW_TS + '-modal-search-result"></div></div>';
        html += '<div class="' + SW_TS + '-modal-view" data-view="trace" style="display:none"><div class="' + SW_TS + '-trace-loading">正在加载调用链...</div></div>';
        body.innerHTML = html;
        body._rawContent = content;
        body._traceId = traceId || '';
        modal.style.display = 'flex';

        var jsonView = body.querySelector('[data-view="json"] pre');
        if (jsonView) {
            jsonView.innerHTML = detectAndPrettyJson(content) || '<span style="color:#86868b">未发现可解析的 JSON 片段</span>';
        }

        // 搜索高亮交互
        var searchInput = body.querySelector('.' + SW_TS + '-modal-search');
        if (searchInput) {
            searchInput.addEventListener('keydown', function (ev) {
                if (ev.key === 'Enter') {
                    var kw = searchInput.value;
                    var r = body.querySelector('.' + SW_TS + '-modal-search-result');
                    if (r) r.innerHTML = '<pre class="' + SW_TS + '-modal-pre">' + highlight(content, kw ? [kw] : []) + '</pre>';
                }
            });
        }
    }
    function closeModal() { var m = document.getElementById(SW_TS + '-modal'); if (m) m.style.display = 'none'; }

    function detectAndPrettyJson(text) {
        if (!text) return '';
        try { return syntaxHighlightJson(JSON.stringify(JSON.parse(text), null, 2)); } catch (e) { }
        var candidates = extractJsonCandidates(text);
        var best = '';
        for (var i = 0; i < candidates.length; i++) {
            try {
                var parsed = JSON.parse(candidates[i]);
                var str = JSON.stringify(parsed, null, 2);
                if (str.length > best.length) best = str;
            } catch (e) { }
        }
        if (best) return syntaxHighlightJson(best);
        return tryGreedyJson(text);
    }
    function tryGreedyJson(text) {
        var firstBrace = text.indexOf('{');
        if (firstBrace < 0) return '';
        var depth = 0, inStr = false, esc = false, lastMatch = -1;
        for (var j = firstBrace; j < text.length; j++) {
            var ch = text[j];
            if (esc) { esc = false; continue; }
            if (ch === '\\' && inStr) { esc = true; continue; }
            if (ch === '"') { inStr = !inStr; continue; }
            if (inStr) continue;
            if (ch === '{') depth++;
            else if (ch === '}') { depth--; if (depth === 0) { lastMatch = j; break; } }
        }
        if (lastMatch < 0) return '';
        try {
            var parsed = JSON.parse(text.substring(firstBrace, lastMatch + 1));
            return syntaxHighlightJson(JSON.stringify(parsed, null, 2));
        } catch (e) { return ''; }
    }
    function extractJsonCandidates(text) {
        var result = [];
        var starts = [];
        for (var i = 0; i < text.length && starts.length < 100; i++) {
            if (text[i] === '{' || text[i] === '[') starts.push(i);
        }
        for (var s = 0; s < starts.length; s++) {
            var start = starts[s];
            var openChar = text[start];
            var closeChar = openChar === '{' ? '}' : ']';
            var depth = 0, inStr = false, esc = false;
            for (var j = start; j < text.length; j++) {
                var ch = text[j];
                if (esc) { esc = false; continue; }
                if (ch === '\\' && inStr) { esc = true; continue; }
                if (ch === '"') { inStr = !inStr; continue; }
                if (inStr) continue;
                if (ch === openChar) depth++;
                else if (ch === closeChar) { depth--; if (depth === 0) { result.push(text.substring(start, j + 1)); break; } }
            }
        }
        return result;
    }
    function syntaxHighlightJson(json) {
        json = json.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
        return json.replace(/("(\\u[a-zA-Z0-9]{4}|\\[^u]|[^\\"])*"(\s*:)?|\b(true|false|null)\b|-?\d+(?:\.\d*)?(?:[eE][+\-]?\d+)?)/g, function (match) {
            var cls = 'sw-json-num';
            if (/^"/.test(match)) {
                if (/:$/.test(match)) cls = 'sw-json-key';
                else cls = 'sw-json-str';
            } else if (/true|false/.test(match)) cls = 'sw-json-bool';
            else if (/null/.test(match)) cls = 'sw-json-null';
            return '<span class="' + cls + '">' + match + '</span>';
        });
    }

    // ==================== Trace (in modal) ====================
    function showTraceErrorView(view, msg) {
        view.innerHTML = '<div class="' + SW_TS + '-trace-loading" style="color:#ff3b30">' +
            '<div style="font-size:24px;margin-bottom:6px;opacity:0.6">⚠️</div>' +
            '<div style="font-size:14px;font-weight:600">服务异常，请重试</div>' +
            '<div style="font-size:11px;color:#86868b;margin-top:4px">' + esc(msg || '') + '</div>' +
            '<button class="' + SW_TS + '-tn-retry" data-act="retry-trace" style="margin-top:10px;padding:5px 14px;border:1px solid rgba(255,59,48,0.3);background:linear-gradient(135deg,#ff3b30,#ff9500);color:#fff;border-radius:8px;font-size:12px;cursor:pointer;font-weight:600">重试</button>' +
        '</div>';
    }
    async function loadTraceIntoModal(traceId, body) {
        body._traceId = traceId;
        var view = body.querySelector('[data-view="trace"]');
        if (!view) return;
        view.innerHTML = '<div class="' + SW_TS + '-trace-loading">正在加载调用链...</div>';
        try {
            var data = await graphql(
                'query queryTrace($traceId: ID!) { trace: queryTrace(traceId: $traceId) { spans { traceId segmentId spanId parentSpanId refs { traceId parentSegmentId parentSpanId type } serviceCode serviceInstanceName endpointName type startTime endTime isError layer tags { key value } logs { time data { key value } } } } }',
                { traceId: traceId }
            );
            // data 为 null 时通常是 503、超时、网络错误等异常情况
            if (!data) {
                showTraceErrorView(view, '后端返回为空（可能是 503 或网络异常）');
                return;
            }
            var spans = (data.trace && data.trace.spans) || [];
            if (!spans.length) {
                view.innerHTML = '<div class="' + SW_TS + '-trace-loading" style="color:#86868b">未找到该 Trace 的调用链</div>';
                return;
            }
            view.innerHTML = renderTraceTreeHTML(spans);
        } catch (e) {
            showTraceErrorView(view, e.message || '');
        }
    }

    function renderTraceTreeHTML(spans) {
        // 构建 span 树
        var byId = {};
        var bySeg = {};
        var roots = [];
        spans.forEach(function (s) {
            s._children = [];
            byId[(s.segmentId || '') + '#' + s.spanId] = s;
            if (s.segmentId) {
                if (!bySeg[s.segmentId]) bySeg[s.segmentId] = [];
                bySeg[s.segmentId].push(s);
            }
        });
        // 找出所有根节点
        spans.forEach(function (s) {
            var parent = null;
            // 1) 优先用 refs[0] 找父(跨 segment / 跨线程)
            if (s.refs && s.refs.length) {
                var r = s.refs[0];
                parent = byId[(r.parentSegmentId || '') + '#' + r.parentSpanId] || null;
            }
            // 2) 否则在同 segment 内按 parentSpanId 找(同段内的本地调用)
            if (!parent && s.segmentId && typeof s.parentSpanId === 'number' && s.parentSpanId >= 0) {
                var segSpans = bySeg[s.segmentId] || [];
                for (var i = 0; i < segSpans.length; i++) {
                    if (segSpans[i].spanId === s.parentSpanId) { parent = segSpans[i]; break; }
                }
            }
            if (parent) parent._children.push(s);
            else roots.push(s);
        });

        var minStart = Infinity, maxEnd = -Infinity;
        spans.forEach(function (s) {
            if (typeof s.startTime === 'number' && s.startTime < minStart) minStart = s.startTime;
            if (typeof s.endTime === 'number' && s.endTime > maxEnd) maxEnd = s.endTime;
        });
        if (!isFinite(minStart)) minStart = 0;
        if (!isFinite(maxEnd)) maxEnd = minStart + 1;
        var totalDur = Math.max(1, maxEnd - minStart);

        var services = {}, errCount = 0;
        spans.forEach(function (s) {
            if (s.serviceCode) services[s.serviceCode] = true;
            if (s.isError) errCount++;
        });
        var svcCount = Object.keys(services).length;

        var svcColorMap = {};
        var palette = ['#ff3b30', '#ff9500', '#34c759', '#007aff', '#af52de', '#ff2d55', '#5ac8fa', '#ffcc00', '#5856d6', '#00c7be'];
        var svcIdx = 0;
        spans.forEach(function (s) {
            var k = s.serviceCode || s.serviceInstanceName || '_';
            if (!svcColorMap[k]) { svcColorMap[k] = palette[svcIdx % palette.length]; svcIdx++; }
        });

        var header = '<div class="' + SW_TS + '-trace-summary">' +
            '<div class="' + SW_TS + '-ts-row">' +
                '<span class="' + SW_TS + '-ts-pill">🧩 ' + spans.length + ' Spans</span>' +
                '<span class="' + SW_TS + '-ts-pill">🌐 ' + svcCount + ' 服务</span>' +
                '<span class="' + SW_TS + '-ts-pill">⏱ ' + totalDur + 'ms</span>' +
                (errCount > 0 ? '<span class="' + SW_TS + '-ts-pill ' + SW_TS + '-ts-err">⚠ ' + errCount + ' 错误</span>' : '<span class="' + SW_TS + '-ts-pill ' + SW_TS + '-ts-ok">✓ 全部成功</span>') +
            '</div>' +
        '</div>';

        var tree = '<div class="' + SW_TS + '-trace-tree">';

        function fmtDur(ms) {
            if (ms < 1000) return ms + 'ms';
            return (ms / 1000).toFixed(2) + 's';
        }

        function walk(node, depth, ancestors) {
            var dur = Math.max(0, (node.endTime || 0) - (node.startTime || 0));
            var offset = ((node.startTime || 0) - minStart);
            var leftPct = (offset / totalDur) * 100;
            var widthPct = Math.max(0.6, (dur / totalDur) * 100);
            if (leftPct + widthPct > 100) widthPct = Math.max(0.6, 100 - leftPct);
            var svcKey = node.serviceCode || node.serviceInstanceName || '_';
            var color = svcColorMap[svcKey] || '#ff3b30';
            var hasError = node.isError;
            var errCls = hasError ? ' ' + SW_TS + '-tn-err' : '';

            var treeCol = '<span class="' + SW_TS + '-tn-tree">';
            for (var a = 0; a < depth; a++) {
                var vertCls = ancestors[a] ? 'has-vert' : 'last-vert';
                treeCol += '<span class="' + SW_TS + '-tn-tree-col ' + vertCls + '"></span>';
            }
            if (depth > 0) {
                treeCol += '<span class="' + SW_TS + '-tn-tree-col connector"></span>';
            }
            treeCol += '</span>';

            var tagHtml = '';
            var tnDbStatement = '';
            var tnDbParams = '';
            if (node.tags && node.tags.length) {
                for (var tti = 0; tti < node.tags.length; tti++) {
                    var ttk = (node.tags[tti].key || '').toLowerCase();
                    if (ttk === 'db.statement') tnDbStatement = node.tags[tti].value || '';
                    else if (ttk === 'db.sql.parameters') tnDbParams = node.tags[tti].value || '';
                }
                tagHtml = '<div class="' + SW_TS + '-tn-tags">' + node.tags.slice(0, 8).map(function (t) {
                    return '<span class="' + SW_TS + '-tn-tag"><b>' + esc(t.key) + '</b>=' + esc(t.value) + '</span>';
                }).join('') + '</div>';
            }
            var tnSqlCopyBtn = tnDbStatement
                ? '<span class="' + SW_TS + '-tn-sqlcopy" data-act="copy-sql" data-sql="' + esc(substituteSqlParams(tnDbStatement, tnDbParams)) + '" title="' + (tnDbParams ? '点击复制填充参数后的完整 SQL' : '未找到 db.sql.parameters，将复制原 SQL') + '">📋</span>'
                : '';

            tree += '<div class="' + SW_TS + '-tn' + errCls + '" data-depth="' + depth + '">' +
                treeCol +
                '<div class="' + SW_TS + '-tn-head">' +
                    '<span class="' + SW_TS + '-tn-dot" style="background:' + color + '"></span>' +
                    '<span class="' + SW_TS + '-tn-svc" style="color:' + color + '">' + esc(node.serviceCode || '-') + '</span>' +
                    (node.layer ? '<span class="' + SW_TS + '-tn-layer">' + esc(node.layer) + '</span>' : '') +
                    '<span class="' + SW_TS + '-tn-ep">' + esc(node.endpointName || node.type || '-') + '</span>' +
                    tnSqlCopyBtn +
                    (hasError ? '<span class="' + SW_TS + '-tn-errpill">ERROR</span>' : '') +
                '</div>' +
                '<div class="' + SW_TS + '-tn-bar-wrap" title="' + fmtDur(dur) + ' (' + Math.round(leftPct) + '% ~ ' + Math.round(leftPct + widthPct) + '%)">' +
                    '<div class="' + SW_TS + '-tn-track"></div>' +
                    '<div class="' + SW_TS + '-tn-bar" style="left:' + leftPct + '%;width:' + widthPct + '%;background:' + (hasError ? '#ff3b30' : color) + '"></div>' +
                '</div>' +
                '<div class="' + SW_TS + '-tn-dur">' + fmtDur(dur) + '</div>' +
                (tagHtml ? tagHtml : '') +
            '</div>';

            var children = node._children || [];
            children.forEach(function (c, idx) {
                var isLast = (idx === children.length - 1);
                var newAncestors = ancestors.concat([!isLast]);
                walk(c, depth + 1, newAncestors);
            });
        }

        roots.forEach(function (r) { walk(r, 0, []); });
        tree += '</div>';

        var legend = '<div class="' + SW_TS + '-trace-legend">';
        Object.keys(svcColorMap).forEach(function (k) {
            if (k === '_') return;
            legend += '<span class="' + SW_TS + '-tn-legend-item"><span class="' + SW_TS + '-tn-dot" style="background:' + svcColorMap[k] + '"></span>' + esc(k) + '</span>';
        });
        legend += '</div>';

        return header + legend + tree;
    }

    // ==================== Open / Close ====================
    function openPlugin() {
        if (document.getElementById(SW_TS + '-wrap')) return;
        // 每次打开时重置标签状态（修复「退出再进入导致标签重复」的问题）
        tabs = [];
        activeTabIndex = -1;
        injectStyle();

        // 隐藏原页面
        originalNodes = [];
        for (var i = 0; i < document.body.children.length; i++) {
            var child = document.body.children[i];
            if (child.id !== SW_TS + '-wrap' &&
                child.id !== SW_TS + '-btn' &&
                child.id !== SW_TS + '-modal' &&
                child.id !== SW_TS + '-diag-modal' &&
                child.tagName.toLowerCase() !== 'script') {
                originalNodes.push({ node: child, display: child.style.display });
                child.style.display = 'none';
            }
        }

        var ui = buildUI();
        document.body.appendChild(ui);
        hideButton();
        // 创建日志详情弹窗容器（必须在 bindEvents 之前，否则弹窗内点击事件无法绑定）
        if (!document.getElementById(SW_TS + '-modal')) {
            var modal = document.createElement('div');
            modal.id = SW_TS + '-modal';
            modal.innerHTML = '<div id="' + SW_TS + '-modal-body"></div>' +
                '<button class="' + SW_TS + '-modal-x" title="关闭">&#10005;</button>';
            document.body.appendChild(modal);
        }
        applyTheme();
        bindEvents();
        loadServices();
        if (!loadTabsState()) addNewTab();
        else {
            renderActiveForm();
            // 恢复当前标签的 items（持久化只存表单字段，items 一般为空，但保留渲染兜底）
            var tab = tabs[activeTabIndex];
            if (tab && tab.contentEl) {
                if (tab.items && tab.items.length) {
                    var parts = [];
                    for (var k = 0; k < tab.items.length; k++) parts.push(renderLogItem(tab.items[k], k));
                    tab.contentEl.insertAdjacentHTML('beforeend', parts.join(''));
                    requestAnimationFrame(function () { adjustItemBodies(tab.contentEl); });
                } else {
                    // 无 items：从 tab 自身恢复 empty（保留「未匹配到日志」「查询失败」等历史状态）
                    restoreEmptyState(tab);
                }
            }
        }
        refreshStats();
    }

    function closePlugin() {
        var ui = document.getElementById(SW_TS + '-wrap');
        if (ui) ui.remove();
        // 恢复原页面
        originalNodes.forEach(function (item) { item.node.style.display = item.display; });
        originalNodes = [];
        showButton();
    }

    // ==================== Init ====================
    function init() {
        injectStyle();
        createToggleButton();
        ensureDiagnosisModal();
        document.addEventListener('keydown', function (event) {
            if (event.key === 'Escape') closeDiagnosisModal();
        });
        if (ensureButtonInterval) clearInterval(ensureButtonInterval);
        ensureButtonInterval = setInterval(function () {
            if (!document.getElementById(SW_TS + '-btn')) createToggleButton();
        }, 1500);
    }

    if (typeof globalThis !== 'undefined' && globalThis.__SW_DIAG_TEST__) {
        globalThis.__SW_DIAG_TEST_API__ = {
            buildDiagnosisDuration: buildDiagnosisDuration,
            normalizeMqeSeries: normalizeMqeSeries,
            pairResourceSeries: pairResourceSeries,
            diagnoseContention: diagnoseContention,
            classifyEndpoint: classifyEndpoint,
            diagnosisTimeKey: diagnosisTimeKey,
            extractNativeDiagnosisContext: extractNativeDiagnosisContext,
            getDiagnosisCapabilities: getDiagnosisCapabilities,
            renderServiceDatalistOptions: renderServiceDatalistOptions,
            renderDiagnosisAlgorithmExplanation: renderDiagnosisAlgorithmExplanation,
            getTabQueryFields: getTabQueryFields
        };
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }
})();
