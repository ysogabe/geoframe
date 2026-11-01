// library_render.mjs 用の MapLibre GL の代用品。地図の見た目は検証できないので、library.js が地図に渡すもの
// （source の GeoJSON・層・吹き出し・マーカー）を記録し、地図のイベント（読み込み・点やクラスタの click）を
// 試験から起こせる程度だけ作る。ネットワークにも DOM にも触らない（Popup/Marker の要素は渡されたものを保持するだけ）。
//
// 使い方: const fake = createFakeMaplibre({bounds, expansionZoom}); vm の context に fake.maplibregl を渡す。
//   fake.maps[i]            作られた Map
//   map.sources[id]         {options, data, setDataCalls: [GeoJSON …]}   addSource / setData の記録
//   map.layers              addLayer に渡した層の配列
//   map.fire(type, event, layerId?)  登録されたハンドラを呼ぶ（layerId を付けた登録だけが対象）
//   map.calls               {resize, easeTo: […], flyTo: […]} の記録
//   fake.popups / fake.markers   作られた Popup（setLngLat・setDOMContent・addTo・remove の記録）/ Marker
//   fake.features           querySourceFeatures が返すクラスタ（試験が差し替える）

export function createFakeMaplibre({bounds = {w: 100, s: 10, e: 120, n: 30}, expansionZoom = 11} = {}){
  const fake = {maps: [], popups: [], markers: [], features: [], bounds: {...bounds}, expansionZoom};

  class FakeMap {
    constructor(options){
      this.options = options;
      this.handlers = [];            // {type, layer, fn}
      this.sources = {};
      this.layers = [];
      this.controls = [];
      this.calls = {resize: 0, easeTo: [], flyTo: []};
      this.canvas = {style: {}};
      this.loaded = false;
      fake.maps.push(this);
    }
    // on(type, fn) と on(type, layerId, fn)。'load' は登録された時点で一度だけ起こす（本物は style の読み込み後）
    on(type, a, b){
      const layer = typeof a === 'function' ? null : a;
      const fn = typeof a === 'function' ? a : b;
      this.handlers.push({type, layer, fn});
      if(type === 'load' && !this.loaded){ this.loaded = true; fn({type: 'load', target: this}); }
      return this;
    }
    fire(type, event = {}, layer = null){
      this.handlers.filter(h => h.type === type && h.layer === layer)
        .forEach(h => h.fn(Object.assign({type, target: this}, event)));
    }
    addControl(control, position){ this.controls.push({control, position}); return this; }
    addSource(id, options){
      this.sources[id] = {options, data: options.data, setDataCalls: []};
      return this;
    }
    addLayer(layer){ this.layers.push(layer); return this; }
    getSource(id){
      const rec = this.sources[id];
      if(!rec) return undefined;
      return {
        setData: data => { rec.data = data; rec.setDataCalls.push(data); },
        getClusterExpansionZoom: clusterId => { rec.expansionAsked = clusterId; return Promise.resolve(fake.expansionZoom); },
      };
    }
    isSourceLoaded(id){ return !!this.sources[id]; }
    querySourceFeatures(){ return fake.features; }
    getBounds(){
      const b = fake.bounds;
      return {getWest: () => b.w, getSouth: () => b.s, getEast: () => b.e, getNorth: () => b.n};
    }
    getCanvas(){ return this.canvas; }
    resize(){ this.calls.resize++; }
    easeTo(o){ this.calls.easeTo.push(o); }
    flyTo(o){ this.calls.flyTo.push(o); }
    jumpTo(){}
  }

  class FakeNavigationControl {
    constructor(options){ this.options = options; }
  }

  class FakePopup {
    constructor(options){
      this.options = options;
      this.lngLat = null; this.content = null; this.map = null; this.removed = false;
      fake.popups.push(this);
    }
    setLngLat(p){ this.lngLat = p; return this; }
    setDOMContent(node){ this.content = node; return this; }
    addTo(map){ this.map = map; return this; }
    remove(){ this.removed = true; return this; }
  }

  class FakeMarker {
    constructor(options = {}){
      this.options = options; this.element = options.element || null;
      this.lngLat = null; this.map = null; this.removed = false;
      fake.markers.push(this);
    }
    setLngLat(p){ this.lngLat = p; return this; }
    addTo(map){ this.map = map; return this; }
    remove(){ this.removed = true; return this; }
    getElement(){ return this.element || {}; }
  }

  fake.maplibregl = {Map: FakeMap, NavigationControl: FakeNavigationControl, Popup: FakePopup, Marker: FakeMarker};
  return fake;
}
