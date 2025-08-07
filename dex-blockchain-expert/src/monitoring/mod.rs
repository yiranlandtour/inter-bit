use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::collections::HashMap;
use tokio::sync::RwLock;
use serde::{Deserialize, Serialize};

// 监控和性能分析工具
// 核心功能：
// 1. 实时性能指标收集
// 2. 火焰图生成
// 3. 延迟追踪
// 4. 资源使用监控

/// 监控系统
pub struct MonitoringSystem {
    metrics_collector: Arc<MetricsCollector>,
    performance_profiler: Arc<PerformanceProfiler>,
    latency_tracker: Arc<LatencyTracker>,
    resource_monitor: Arc<ResourceMonitor>,
    alert_manager: Arc<AlertManager>,
}

impl MonitoringSystem {
    pub fn new() -> Self {
        Self {
            metrics_collector: Arc::new(MetricsCollector::new()),
            performance_profiler: Arc::new(PerformanceProfiler::new()),
            latency_tracker: Arc::new(LatencyTracker::new()),
            resource_monitor: Arc::new(ResourceMonitor::new()),
            alert_manager: Arc::new(AlertManager::new()),
        }
    }

    pub async fn start(&self) {
        // 启动各个监控组件
        let collector = self.metrics_collector.clone();
        tokio::spawn(async move {
            collector.start_collection().await;
        });

        let profiler = self.performance_profiler.clone();
        tokio::spawn(async move {
            profiler.start_profiling().await;
        });

        let tracker = self.latency_tracker.clone();
        tokio::spawn(async move {
            tracker.start_tracking().await;
        });

        let monitor = self.resource_monitor.clone();
        tokio::spawn(async move {
            monitor.start_monitoring().await;
        });
    }

    pub async fn get_dashboard_data(&self) -> DashboardData {
        DashboardData {
            metrics: self.metrics_collector.get_current_metrics().await,
            performance: self.performance_profiler.get_profile_summary().await,
            latencies: self.latency_tracker.get_latency_stats().await,
            resources: self.resource_monitor.get_resource_usage().await,
            alerts: self.alert_manager.get_active_alerts().await,
        }
    }
}

/// 指标收集器
pub struct MetricsCollector {
    metrics: Arc<RwLock<HashMap<String, Metric>>>,
    history: Arc<RwLock<Vec<MetricSnapshot>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub tags: HashMap<String, String>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSnapshot {
    pub timestamp: u64,
    pub metrics: HashMap<String, f64>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self {
            metrics: Arc::new(RwLock::new(HashMap::new())),
            history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn record(&self, name: String, value: f64, unit: String, tags: HashMap<String, String>) {
        let metric = Metric {
            name: name.clone(),
            value,
            unit,
            tags,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };

        self.metrics.write().await.insert(name, metric);
    }

    pub async fn increment(&self, name: String, delta: f64) {
        let mut metrics = self.metrics.write().await;
        let metric = metrics.entry(name.clone()).or_insert(Metric {
            name: name.clone(),
            value: 0.0,
            unit: "count".to_string(),
            tags: HashMap::new(),
            timestamp: 0,
        });
        metric.value += delta;
        metric.timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
    }

    pub async fn gauge(&self, name: String, value: f64) {
        self.record(name, value, "gauge".to_string(), HashMap::new()).await;
    }

    pub async fn histogram(&self, name: String, value: f64) {
        // 简化的直方图实现
        let bucket_name = format!("{}_bucket", name);
        self.increment(bucket_name, 1.0).await;
        
        let sum_name = format!("{}_sum", name);
        self.increment(sum_name, value).await;
    }

    async fn start_collection(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        
        loop {
            interval.tick().await;
            
            // 创建快照
            let snapshot = {
                let metrics = self.metrics.read().await;
                let mut snapshot_metrics = HashMap::new();
                
                for (name, metric) in metrics.iter() {
                    snapshot_metrics.insert(name.clone(), metric.value);
                }
                
                MetricSnapshot {
                    timestamp: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs(),
                    metrics: snapshot_metrics,
                }
            };
            
            // 保存历史
            let mut history = self.history.write().await;
            history.push(snapshot);
            
            // 限制历史大小
            if history.len() > 1000 {
                history.remove(0);
            }
        }
    }

    pub async fn get_current_metrics(&self) -> HashMap<String, Metric> {
        self.metrics.read().await.clone()
    }

    pub async fn get_history(&self, duration: Duration) -> Vec<MetricSnapshot> {
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() - duration.as_secs();
        
        self.history
            .read()
            .await
            .iter()
            .filter(|s| s.timestamp >= cutoff)
            .cloned()
            .collect()
    }
}

/// 性能分析器
pub struct PerformanceProfiler {
    profiles: Arc<RwLock<HashMap<String, Profile>>>,
    sampling_rate: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub samples: Vec<Sample>,
    pub call_graph: CallGraph,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    pub timestamp: u64,
    pub stack_trace: Vec<String>,
    pub cpu_time_us: u64,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallGraph {
    pub nodes: HashMap<String, CallNode>,
    pub edges: Vec<CallEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallNode {
    pub function: String,
    pub self_time_us: u64,
    pub total_time_us: u64,
    pub call_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallEdge {
    pub from: String,
    pub to: String,
    pub count: u64,
}

impl PerformanceProfiler {
    pub fn new() -> Self {
        Self {
            profiles: Arc::new(RwLock::new(HashMap::new())),
            sampling_rate: 100, // 100 Hz
        }
    }

    pub async fn start_profiling(&self) {
        // 模拟性能分析
        let mut interval = tokio::time::interval(Duration::from_millis(1000 / self.sampling_rate as u64));
        
        loop {
            interval.tick().await;
            
            // 收集样本
            let sample = Sample {
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                stack_trace: vec![
                    "main".to_string(),
                    "consensus::run".to_string(),
                    "state_machine::execute".to_string(),
                ],
                cpu_time_us: rand::random::<u64>() % 1000,
                memory_bytes: rand::random::<u64>() % 1000000,
            };
            
            // 更新profile
            let mut profiles = self.profiles.write().await;
            let profile = profiles.entry("default".to_string()).or_insert(Profile {
                name: "default".to_string(),
                samples: Vec::new(),
                call_graph: CallGraph {
                    nodes: HashMap::new(),
                    edges: Vec::new(),
                },
            });
            
            profile.samples.push(sample);
            
            // 限制样本数量
            if profile.samples.len() > 10000 {
                profile.samples.remove(0);
            }
        }
    }

    pub async fn get_profile_summary(&self) -> ProfileSummary {
        let profiles = self.profiles.read().await;
        
        let mut total_cpu_time = 0;
        let mut total_memory = 0;
        let mut function_times = HashMap::new();
        
        for profile in profiles.values() {
            for sample in &profile.samples {
                total_cpu_time += sample.cpu_time_us;
                total_memory += sample.memory_bytes;
                
                for function in &sample.stack_trace {
                    *function_times.entry(function.clone()).or_insert(0) += sample.cpu_time_us;
                }
            }
        }
        
        ProfileSummary {
            total_cpu_time_us: total_cpu_time,
            total_memory_bytes: total_memory,
            hot_functions: function_times
                .into_iter()
                .map(|(name, time)| HotFunction { name, cpu_time_us: time })
                .collect(),
        }
    }

    pub async fn generate_flamegraph(&self) -> String {
        // 生成火焰图数据（简化版）
        let profiles = self.profiles.read().await;
        let mut flamegraph_data = String::new();
        
        for profile in profiles.values() {
            for sample in &profile.samples {
                let stack = sample.stack_trace.join(";");
                flamegraph_data.push_str(&format!("{} {}\n", stack, sample.cpu_time_us));
            }
        }
        
        flamegraph_data
    }
}

/// 延迟追踪器
pub struct LatencyTracker {
    traces: Arc<RwLock<HashMap<String, Vec<Span>>>>,
    active_spans: Arc<RwLock<HashMap<String, Span>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    pub parent_id: Option<String>,
    pub operation: String,
    pub start_time: Instant,
    pub duration: Option<Duration>,
    pub tags: HashMap<String, String>,
}

impl LatencyTracker {
    pub fn new() -> Self {
        Self {
            traces: Arc::new(RwLock::new(HashMap::new())),
            active_spans: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn start_span(&self, operation: String) -> String {
        let span_id = format!("{:x}", rand::random::<u64>());
        let trace_id = format!("{:x}", rand::random::<u64>());
        
        let span = Span {
            trace_id: trace_id.clone(),
            span_id: span_id.clone(),
            parent_id: None,
            operation,
            start_time: Instant::now(),
            duration: None,
            tags: HashMap::new(),
        };
        
        self.active_spans.write().await.insert(span_id.clone(), span);
        
        span_id
    }

    pub async fn end_span(&self, span_id: String) {
        let mut active = self.active_spans.write().await;
        
        if let Some(mut span) = active.remove(&span_id) {
            span.duration = Some(span.start_time.elapsed());
            
            let mut traces = self.traces.write().await;
            traces.entry(span.trace_id.clone())
                .or_insert_with(Vec::new)
                .push(span);
        }
    }

    async fn start_tracking(&self) {
        // 清理旧的trace
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        
        loop {
            interval.tick().await;
            
            let mut traces = self.traces.write().await;
            
            // 保留最近1000个trace
            if traces.len() > 1000 {
                let to_remove: Vec<String> = traces.keys()
                    .take(traces.len() - 1000)
                    .cloned()
                    .collect();
                
                for key in to_remove {
                    traces.remove(&key);
                }
            }
        }
    }

    pub async fn get_latency_stats(&self) -> LatencyStats {
        let traces = self.traces.read().await;
        
        let mut operation_latencies: HashMap<String, Vec<u64>> = HashMap::new();
        
        for trace in traces.values() {
            for span in trace {
                if let Some(duration) = span.duration {
                    operation_latencies
                        .entry(span.operation.clone())
                        .or_insert_with(Vec::new)
                        .push(duration.as_micros() as u64);
                }
            }
        }
        
        let mut stats = HashMap::new();
        
        for (operation, latencies) in operation_latencies {
            if !latencies.is_empty() {
                let sum: u64 = latencies.iter().sum();
                let avg = sum / latencies.len() as u64;
                
                let mut sorted = latencies.clone();
                sorted.sort();
                
                let p50 = sorted[sorted.len() / 2];
                let p95 = sorted[sorted.len() * 95 / 100];
                let p99 = sorted[sorted.len() * 99 / 100];
                
                stats.insert(operation, OperationLatency {
                    avg_us: avg,
                    p50_us: p50,
                    p95_us: p95,
                    p99_us: p99,
                    count: latencies.len() as u64,
                });
            }
        }
        
        LatencyStats { operations: stats }
    }
}

/// 资源监控器
pub struct ResourceMonitor {
    usage_history: Arc<RwLock<Vec<ResourceUsage>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceUsage {
    pub timestamp: u64,
    pub cpu_percent: f64,
    pub memory_bytes: u64,
    pub disk_io_bytes: u64,
    pub network_io_bytes: u64,
    pub goroutines: u32,
    pub file_descriptors: u32,
}

impl ResourceMonitor {
    pub fn new() -> Self {
        Self {
            usage_history: Arc::new(RwLock::new(Vec::new())),
        }
    }

    async fn start_monitoring(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        
        loop {
            interval.tick().await;
            
            let usage = self.collect_usage().await;
            
            let mut history = self.usage_history.write().await;
            history.push(usage);
            
            // 保留最近1小时的数据
            if history.len() > 720 {
                history.remove(0);
            }
        }
    }

    async fn collect_usage(&self) -> ResourceUsage {
        // 模拟资源使用收集
        ResourceUsage {
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            cpu_percent: rand::random::<f64>() * 100.0,
            memory_bytes: rand::random::<u64>() % (4 * 1024 * 1024 * 1024), // 4GB max
            disk_io_bytes: rand::random::<u64>() % (100 * 1024 * 1024), // 100MB/s max
            network_io_bytes: rand::random::<u64>() % (1000 * 1024 * 1024), // 1GB/s max
            goroutines: rand::random::<u32>() % 10000,
            file_descriptors: rand::random::<u32>() % 65536,
        }
    }

    pub async fn get_resource_usage(&self) -> ResourceUsageSummary {
        let history = self.usage_history.read().await;
        
        if history.is_empty() {
            return ResourceUsageSummary::default();
        }
        
        let current = history.last().unwrap().clone();
        
        let avg_cpu = history.iter().map(|u| u.cpu_percent).sum::<f64>() / history.len() as f64;
        let max_memory = history.iter().map(|u| u.memory_bytes).max().unwrap_or(0);
        
        ResourceUsageSummary {
            current,
            avg_cpu_percent: avg_cpu,
            max_memory_bytes: max_memory,
        }
    }
}

/// 告警管理器
pub struct AlertManager {
    alerts: Arc<RwLock<Vec<Alert>>>,
    rules: Arc<RwLock<Vec<AlertRule>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    pub severity: AlertSeverity,
    pub message: String,
    pub timestamp: u64,
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

pub struct AlertRule {
    pub name: String,
    pub condition: Box<dyn Fn(&DashboardData) -> bool + Send + Sync>,
    pub severity: AlertSeverity,
    pub message: String,
}

impl AlertManager {
    pub fn new() -> Self {
        Self {
            alerts: Arc::new(RwLock::new(Vec::new())),
            rules: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn add_rule(&self, rule: AlertRule) {
        self.rules.write().await.push(rule);
    }

    pub async fn check_rules(&self, data: &DashboardData) {
        let rules = self.rules.read().await;
        
        for rule in rules.iter() {
            if (rule.condition)(data) {
                self.trigger_alert(rule.severity.clone(), rule.message.clone()).await;
            }
        }
    }

    pub async fn trigger_alert(&self, severity: AlertSeverity, message: String) {
        let alert = Alert {
            id: format!("{:x}", rand::random::<u64>()),
            severity,
            message,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            resolved: false,
        };
        
        self.alerts.write().await.push(alert);
    }

    pub async fn get_active_alerts(&self) -> Vec<Alert> {
        self.alerts
            .read()
            .await
            .iter()
            .filter(|a| !a.resolved)
            .cloned()
            .collect()
    }
}

// 数据结构定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardData {
    pub metrics: HashMap<String, Metric>,
    pub performance: ProfileSummary,
    pub latencies: LatencyStats,
    pub resources: ResourceUsageSummary,
    pub alerts: Vec<Alert>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub total_cpu_time_us: u64,
    pub total_memory_bytes: u64,
    pub hot_functions: Vec<HotFunction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotFunction {
    pub name: String,
    pub cpu_time_us: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyStats {
    pub operations: HashMap<String, OperationLatency>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationLatency {
    pub avg_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceUsageSummary {
    pub current: ResourceUsage,
    pub avg_cpu_percent: f64,
    pub max_memory_bytes: u64,
}

impl Default for ResourceUsage {
    fn default() -> Self {
        Self {
            timestamp: 0,
            cpu_percent: 0.0,
            memory_bytes: 0,
            disk_io_bytes: 0,
            network_io_bytes: 0,
            goroutines: 0,
            file_descriptors: 0,
        }
    }
}