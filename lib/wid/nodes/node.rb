module Wid
  module Nodes
    class Node
      def initialize(*); end

      def accept(visitor) = visitor.visit(self)

      def inspect
        attrs = instance_variables.map do |var|
          "#{var[1..-1]}=#{instance_variable_get(var).inspect}"
        end
        "#{self.class.name.split("::").last}(#{attrs.join(" ")})"
      end
    end
  end
end
