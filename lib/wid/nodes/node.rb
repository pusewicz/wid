# typed: strict

module Wid
  module Nodes
    class Node
      extend T::Sig

      sig { returns(String) }
      def inspect
        attrs = instance_variables.map do |var|
          "#{var[1..]}=#{instance_variable_get(var).inspect}"
        end
        "#{T.must(self.class.name).split("::").last}(#{attrs.join(" ")})"
      end

      sig { returns(T::Array[T.untyped]) }
      def children = []

      sig { returns(T::Hash[Symbol, T.class_of(Node)]) }
      def to_hash
        {class: self.class}
      end
    end
  end
end
